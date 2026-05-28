#!/usr/bin/env python3
"""Derive Hush's bundled Silero VAD ONNX from the upstream v5.1.2 release.

The upstream model wraps its 16kHz / 8kHz inference paths in an ONNX `If` op
gated on `sr == 16000`. `tract-onnx 0.22.1` analyses both branches of an
`If` strictly, and the dead 8kHz branch has ops whose shapes conflict with
the inputs we pin for the 16kHz path, so the model fails to load (see
`learnings.md` for the dance of why, and `docs/vad-hallucination-gate-plan.md`
Task 3 for the original blocker).

There are also six *inner* `If` ops nested inside the 16kHz branch — three in
the decoder and three more inside the LSTM dispatch subgraph — gated on
shape-derived comparisons (`Gather(Shape(x)) == k`). At the input shape we
pin ([1, 512] audio + [2, 1, 128] state) every one of these inner conditions
becomes a runtime-invariant constant, but tract still strict-analyses both
branches and chokes on rank mismatches between them.

The fix is to specialize for 16kHz offline. The plan-of-record was to
substitute `sr` with a constant initializer and let `onnxsim` constant-fold
the `If` away. `onnxsim 0.6.3` crashes during that fold on the upstream model
("Input … is undefined!" — it fails to inline the `If` subgraph cleanly).
And it wouldn't have helped with the inner Ifs anyway, which depend on
shape inference that the upstream graph metadata doesn't carry forward.

So this script does the work directly:

  1. Splice the top-level `If`'s `then_branch` (the 16kHz path) into the
     main graph; drop the now-orphan `Equal_0` / `Constant_0` and the `sr`
     input; pin `input` / `state` to their fixed shapes.
  2. Iterate: for each remaining `If` in the main graph, run the partially-
     spliced model in onnxruntime (graph optimisation disabled — ORT's
     optimiser otherwise re-externalises constants and breaks the session),
     read the condition tensor's runtime value, and splice the corresponding
     branch in. Repeat until no `If` ops remain.

Resulting model:
  - Inputs:  `input` [1, 512] f32 (audio), `state` [2, 1, 128] f32 (LSTM)
  - Outputs: `output` [1, 1] f32 (speech prob), `stateN` [2, 1, 128] f32
  - 212 nodes, zero `If` ops at any nesting level
  - SHA256 stable across runs (verified across two clean re-derivations)

Re-run when bumping Silero versions. Requires:
  pip install onnx onnxruntime numpy requests
"""
import copy
import hashlib
import sys
from pathlib import Path

try:
    import numpy as np
    import onnx
    from onnx import helper, TensorProto
    import onnxruntime as ort
    import requests
except ImportError as e:
    print(
        f"Missing dependency: {e}\n"
        "Install with: pip install onnx onnxruntime numpy requests",
        file=sys.stderr,
    )
    sys.exit(1)

UPSTREAM_URL = (
    "https://github.com/snakers4/silero-vad/raw/v5.1.2/"
    "src/silero_vad/data/silero_vad.onnx"
)
TARGET_SR = 16_000
INPUT_SHAPE = [1, 512]
STATE_SHAPE = [2, 1, 128]
REPO_ROOT = Path(__file__).resolve().parent.parent
OUT_PATH = REPO_ROOT / "src-tauri" / "assets" / "silero_vad.onnx"


def topo_sort(model, candidate_nodes):
    """Topological order over `candidate_nodes` against `model`'s initializers
    + inputs. Defensive — branch nodes carry their original order but the
    splice can interleave a Constant after a consumer."""
    known = set()
    for init in model.graph.initializer:
        known.add(init.name)
    for inp in model.graph.input:
        known.add(inp.name)
    remaining = list(candidate_nodes)
    ordered = []
    progress = True
    while remaining and progress:
        progress = False
        next_remaining = []
        for n in remaining:
            if all((i in known or i == "") for i in n.input):
                ordered.append(n)
                for o in n.output:
                    known.add(o)
                progress = True
            else:
                next_remaining.append(n)
        remaining = next_remaining
    if remaining:
        bad = remaining[0]
        miss = [i for i in bad.input if i not in known and i != ""]
        raise RuntimeError(
            f"Topo sort stuck: {bad.op_type} {bad.name} still needs {miss}"
        )
    return ordered


def splice_if(model, target_name, take_branch):
    """In-place: replace `target_name` If node with its `take_branch` nodes,
    rewiring the If's outputs to the branch's terminal outputs via Identity."""
    g = model.graph
    if_idx = None
    if_node = None
    for idx, n in enumerate(g.node):
        if n.name == target_name and n.op_type == "If":
            if_idx = idx
            if_node = n
            break
    if if_node is None:
        return False
    sub = None
    for attr in if_node.attribute:
        if attr.name == take_branch:
            sub = attr.g
            break
    if sub is None:
        return False
    pre = list(g.node[:if_idx])
    post = list(g.node[if_idx + 1:])
    rewires = [
        helper.make_node(
            "Identity",
            [bo.name],
            [io],
            name=f"BranchOut_{io}",
        )
        for bo, io in zip(sub.output, if_node.output)
    ]
    new_nodes = pre + list(sub.node) + rewires + post
    for init in sub.initializer:
        g.initializer.append(init)
    ordered = topo_sort(model, new_nodes)
    del g.node[:]
    g.node.extend(ordered)
    return True


def probe_conditions(model):
    """Run a deep copy of `model` through onnxruntime (with all graph
    optimisations disabled, because ORT's optimiser otherwise re-flags
    initializers as external-data and breaks the session) to discover the
    bool value of each remaining `If`'s condition tensor. Inputs are zero;
    every observed condition is shape-derived and so does not depend on
    audio content."""
    m2 = copy.deepcopy(model)
    targets = [(n.name, n.input[0]) for n in m2.graph.node if n.op_type == "If"]
    existing_outs = {o.name for o in m2.graph.output}
    added = []
    for nm, cond in targets:
        if cond not in existing_outs:
            m2.graph.output.append(
                helper.make_tensor_value_info(cond, TensorProto.BOOL, None)
            )
            added.append((nm, cond))
    opts = ort.SessionOptions()
    opts.graph_optimization_level = ort.GraphOptimizationLevel.ORT_DISABLE_ALL
    sess = ort.InferenceSession(
        m2.SerializeToString(), sess_options=opts, providers=["CPUExecutionProvider"]
    )
    audio = np.zeros(tuple(INPUT_SHAPE), dtype=np.float32)
    state = np.zeros(tuple(STATE_SHAPE), dtype=np.float32)
    out_names = [o.name for o in sess.get_outputs()]
    out = sess.run(out_names, {"input": audio, "state": state})
    name_to_val = dict(zip(out_names, out))
    result = {}
    for nm, cond in added:
        v = name_to_val[cond]
        result[nm] = bool(np.atleast_1d(v).flatten()[0])
    return result


def specialize_top_level_if(model):
    """First splice: the top-level `sr == 16000` gate."""
    g = model.graph
    if_node = None
    if_idx = None
    for idx, n in enumerate(g.node):
        if n.op_type == "If":
            if_node = n
            if_idx = idx
            break
    if if_node is None:
        sys.exit("Upstream model has no top-level `If` — layout changed; re-investigate.")
    then_g = next((a.g for a in if_node.attribute if a.name == "then_branch"), None)
    if then_g is None:
        sys.exit("Top-level If has no `then_branch` — re-investigate.")

    pre = [n for n in g.node[:if_idx] if n.name not in ("Constant_0", "Equal_0")]
    post = list(g.node[if_idx + 1:])
    rewires = [
        helper.make_node(
            "Identity",
            [bo.name],
            [io],
            name=f"BranchOut_{io}",
        )
        for bo, io in zip(then_g.output, if_node.output)
    ]
    new_nodes = pre + list(then_g.node) + rewires + post
    for init in then_g.initializer:
        g.initializer.append(init)
    ordered = topo_sort(model, new_nodes)
    del g.node[:]
    g.node.extend(ordered)

    # Drop sr input and pin shapes.
    for s in [i for i in g.input if i.name == "sr"]:
        g.input.remove(s)
    for inp in g.input:
        if inp.name == "input":
            del inp.type.tensor_type.shape.dim[:]
            for v in INPUT_SHAPE:
                d = inp.type.tensor_type.shape.dim.add()
                d.dim_value = v
        elif inp.name == "state":
            del inp.type.tensor_type.shape.dim[:]
            for v in STATE_SHAPE:
                d = inp.type.tensor_type.shape.dim.add()
                d.dim_value = v


def main() -> None:
    print(f"Downloading upstream Silero v5.1.2 from {UPSTREAM_URL}")
    r = requests.get(UPSTREAM_URL, timeout=60)
    r.raise_for_status()
    print(f"  upstream size: {len(r.content):,} bytes")
    model = onnx.load_from_string(r.content)

    print("Splicing top-level If (sr == 16000) → then_branch")
    specialize_top_level_if(model)

    print("Iteratively resolving inner Ifs by runtime probe")
    iteration = 0
    while True:
        iteration += 1
        if iteration > 20:
            sys.exit("Too many splice iterations — infinite loop or model layout changed.")
        if_names = [n.name for n in model.graph.node if n.op_type == "If"]
        if not if_names:
            break
        conds = probe_conditions(model)
        for nm in if_names:
            cond = conds.get(nm, True)
            branch = "then_branch" if cond else "else_branch"
            print(f"  iter {iteration}: splice {nm} → {branch} (cond={cond})")
            splice_if(model, nm, branch)

    onnx.checker.check_model(model)
    print(f"Final graph: {len(model.graph.node)} nodes; 0 If ops")

    OUT_PATH.parent.mkdir(parents=True, exist_ok=True)
    with open(OUT_PATH, "wb") as f:
        f.write(model.SerializeToString())
    out_size = OUT_PATH.stat().st_size
    print(f"Wrote {OUT_PATH} ({out_size:,} bytes)")
    print(f"  inputs:  {[i.name for i in model.graph.input]}")
    print(f"  outputs: {[o.name for o in model.graph.output]}")

    sha = hashlib.sha256(OUT_PATH.read_bytes()).hexdigest()
    print(f"SHA256: {sha}")
    print("\nPaste this SHA into the `SILERO_VAD_SHA256` constant in `src-tauri/src/vad/onnx.rs`.")


if __name__ == "__main__":
    main()
