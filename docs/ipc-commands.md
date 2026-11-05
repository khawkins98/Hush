# Add or change an IPC command

Keep the Rust wire contract and frontend call sites aligned. Paths below are
relative to the repository root. For setup and general validation use
[Developing Hush](./developing.md); contribution and source-discipline requirements
remain in [CONTRIBUTING](../CONTRIBUTING.md).

A command has four coupled representations: Rust types/handler, Tauri
registration, TypeScript shape/call site and the Playwright mock. Compilation
alone does not prove their wire shapes agree.

## 1. Define the Rust types

In `src-tauri/src/ipc/commands/mod.rs` (or one of the existing domain submodules — `commands/meeting.rs`, `commands/models.rs`, `commands/macos.rs` — extracted under #82), add the request/response struct. New domain-cohesive command groups should follow the same submodule pattern. Apply `#[serde(rename_all = "camelCase")]` so the wire shape matches JS conventions:

```rust
#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MyCommandResult {
    pub example_field: String,
    pub another_field: u32,  // serialises as `anotherField`
}
```

Define the command itself with `#[tauri::command]`:

```rust
#[tauri::command]
pub async fn my_command(state: State<'_, AppState>, arg: String) -> IpcResult<MyCommandResult> {
    // ...
}
```

Errors should map to a variant of `IpcError` so the frontend's `formatErrorDisplay` (in `src/lib/errors.ts`) can produce a tailored `{ headline, hint?, details? }` for the shared `ErrorDisplay` component. Adding a new error variant means updating that mapper — see step 4 below.

## 2. Register the command in the Tauri builder

In `src-tauri/src/lib.rs`, add the command to the `tauri::generate_handler![...]` macro:

```rust
.invoke_handler(tauri::generate_handler![
    // ... existing commands ...
    ipc::commands::my_command,                       // for top-level commands
    ipc::commands::meeting::meeting_my_command,      // for submodule commands
])
```

The macro looks for `__cmd__<name>` siblings in the same module as the function. Use the **full module path** (`ipc::commands::my_command`, or `ipc::commands::meeting::…` for submodule commands), not a re-export — `pub use` does not carry the macro's hidden symbol. (See `learnings.md` 2026-04-25 for the trap that cost us once.)

## 3. Add the TypeScript type and call site

Declare a TypeScript type that matches the Rust struct's serialised shape. Two conventional locations:

- **Shared across the page and panel components.** Add it to `src/lib/types.ts` (the post-#84 split moved cross-cutting types here) and `import type { MyCommandResult } from "$lib/types";` from both the parent page and any child panel that needs it.
- **Used only by `+page.svelte`.** Declare it inline in the `<script>` block at the top of the page.

```typescript
export type MyCommandResult = {
    exampleField: string;
    anotherField: number;
};
```

Then `invoke<MyCommandResult>("my_command", { arg: "..." })`.

**The shape must match exactly.** A typo here (`example_field` instead of `exampleField`, `string` instead of `number`) compiles fine and produces silent `undefined`s at runtime. The Playwright e2e suite catches this *only* if a spec asserts on the field — it's not automatic.

## 4. Update the Playwright mock

In `tests/e2e/_mock.ts`, add a default handler so e2e tests have a stub for the new command:

```typescript
my_command: (args: unknown) => {
    const a = args as { arg: string };
    return { exampleField: a.arg, anotherField: 42 };
},
```

The mock's field shape must mirror the Rust struct — same camelCase names, same types. A round-5 review caught a regression where the model-card mock had `sizeBytes` / `speed` fields while the Rust side serialised `sizeMb` / `speedRating`; tests passed by luck.

Mocks are serialized via `toString()` and rebuilt in the page context, so they
cannot capture closure variables. Route per-test counters through
`page.exposeFunction`.

If the new command needs error simulation in a test, override at the spec level (`installMocks(page, { my_command: () => { throw { kind: "settings", message: "..." } } })`).

If the command introduces a new `IpcError` variant, also update `formatErrorDisplay` in `src/lib/errors.ts` so the user gets tailored copy in the shared `ErrorDisplay` instead of the generic default.

## Window permissions

Settings UI commands run in the main window. Any required Tauri plugin
permissions belong in `src-tauri/capabilities/default.json`. The `hud`,
`menu-bar` and `debug` windows each have a separate capability file and need
explicit grants if they start invoking those plugin commands. Custom
`#[tauri::command]` functions do not need permission entries; Tauri plugin
commands (autostart, clipboard, etc.) do.

## Verifying

After all four steps:

```bash
(cd src-tauri && cargo build --lib)  # Rust struct + command compile
(cd src-tauri && cargo test --lib)   # IPC tests still pass
npm run check                       # TypeScript types compile
npm run test:e2e                     # Mocks work end-to-end
npm run tauri dev                    # Real backend roundtrip
```

If any of these surface a mismatch, fix at the appropriate layer above. The four places are coupled; CI catches Rust-only and TS-only breaks but cannot catch type-shape mismatches between them — that's a hands-on smoke responsibility.

Exercise the changed command in the running app and inspect its returned fields;
a successful launch alone does not verify the wire contract. Startup changes
also require the [dev-launch smoke](./developing.md#dev-launch-smoke).
