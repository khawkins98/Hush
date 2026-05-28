# Bundled assets

## `silero_vad.onnx`

**Source:** [snakers4/silero-vad v5.1.2](https://github.com/snakers4/silero-vad/releases/tag/v5.1) (MIT licensed).

**Derivation:** This is a 16kHz-specialized variant of the upstream model.

The upstream v5 model wraps its 16kHz / 8kHz inference paths in an ONNX
`If` op gated on `sr == 16000`. `tract-onnx 0.22.1` analyses both branches
of an `If` strictly, and the dead 8kHz branch has ops that conflict with
the inputs we pin for the 16kHz path. There are also six *inner* `If` ops
nested inside the 16kHz branch — three in the decoder and three more inside
the LSTM dispatch — gated on shape-derived comparisons. At our pinned input
shape ([1, 512] audio + [2, 1, 128] state) every one of those inner
conditions reduces to a runtime constant, but tract still strict-analyses
both branches and chokes on rank mismatches between them.

The derivation script (`scripts/build-silero-vad-onnx.py`) eliminates every
`If` op offline: it splices the top-level `then_branch` (the 16kHz path)
into the main graph, drops the `sr` input, then iteratively probes each
remaining inner `If`'s condition at runtime (via onnxruntime with all graph
optimisation disabled — ORT's optimiser otherwise re-flags initializers as
external-data and breaks the resulting session), and splices the chosen
branch in. The result has 212 nodes and zero `If` ops at any nesting level,
~1.28 MB vs the upstream 2.33 MB.

Note: the plan-of-record had been to substitute `sr` with a constant
initializer and let `onnxsim` constant-fold the `If` away. `onnxsim 0.6.3`
(current release on PyPI as of 2026-05) crashes during that fold on the
upstream model (dangling subgraph input references) and would not have
handled the inner Ifs anyway. The current script does the work directly.

See `learnings.md` 2026-05-28 for the full reasoning.

**Reproducing the build:**
```bash
python3 -m venv /tmp/vad-venv && source /tmp/vad-venv/bin/activate
pip install onnx onnxruntime numpy requests
python3 scripts/build-silero-vad-onnx.py
```

This downloads the upstream release, specializes, and writes the result
to this directory. The script prints the SHA256 of the result — paste it
into the `SILERO_VAD_SHA256` constant in `src-tauri/src/vad/onnx.rs`.
