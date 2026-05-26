# Frontend E2E tests (Playwright)

Browser-driven smoke tests for the Hush frontend. Drives the SvelteKit
dev server in **mocked-Tauri mode** — `vite.config.js` swaps
`@tauri-apps/api/{core,event,app}` + `plugin-shell` for the in-tree
stubs in `tests/e2e/setup/` when `HUSH_E2E=1` is set, so tests run in
plain Chromium without Tauri's runtime.

> The same stubs power the **`npm run dev` browser playground**
> (`HUSH_MOCK=1`). The difference is the seed: tests use `_mock.ts`
> (minimal, deterministic, throw-on-unmocked); `npm run dev` uses
> `setup/mock-defaults.ts` (populated, forgiving). See
> [`docs/developing.md`](../../docs/developing.md).

These tests **cannot** validate full-stack flows (real IPC round-trips,
HUD lifecycle, hotkey registration, real audio, real model download).
Those live behind the [tauri-driver follow-up (#57)][57]. Path A here
catches frontend regressions cheaply: the round-4 reviewer's modal
a11y findings, error-copy drift, retry-race UX, and aria-attribute
bugs all live in the layer this suite covers.

[57]: https://github.com/khawkins98/Hush/issues/57

## Run locally

```sh
# Install Chromium once (skip if already installed)
npx playwright install chromium

# All specs, headless
npm run test:e2e

# Interactive UI runner — handy when authoring new tests
npm run test:e2e:ui
```

## Layout

```
tests/e2e/
  setup/
    core-stub.ts          # replaces @tauri-apps/api/core (HUSH_E2E or HUSH_MOCK)
    event-stub.ts         # replaces @tauri-apps/api/event
    app-stub.ts           # replaces @tauri-apps/api/app
    shell-stub.ts         # replaces @tauri-apps/plugin-shell
    mock-defaults.ts      # populated seed for `npm run dev` (HUSH_MOCK);
                          # no-ops under Playwright
  _mock.ts                # `installMocks(page, overrides?)` — default
                          # invoke handlers + `fireEvent(page, name, payload)`
  *.spec.ts               # actual specs
  README.md
```

## Authoring a new spec

1. Import `installMocks` from `./_mock`.
2. Call `await installMocks(page, overrides)` **before** `page.goto(...)`.
3. Use `fireEvent(page, 'audio:level', 0.5)` to simulate
   backend-emitted events.
4. Default `invoke` handlers in `_mock.ts` give every test a working
   app baseline. Only override the commands your assertions touch —
   if every test redeclared all of them the fixtures would drift.
5. Unmocked invokes throw on purpose. If a new app-side `invoke(...)`
   call site appears, the failing test points at the missing mock
   immediately rather than passing with `undefined`.

### Example

```ts
import { expect, test } from "@playwright/test";
import { installMocks, fireEvent } from "./_mock";

test("model picker shows download progress", async ({ page }) => {
  await installMocks(page, {
    model_list: () => [/* ... a model that's not downloaded */],
    model_download: () => undefined, // start succeeds
  });
  await page.goto("/");

  await page.getByRole("button", { name: "Download Whisper Base" }).click();
  await fireEvent(page, "model:download-progress", {
    id: "whisper-base",
    received: 1024,
    total: 4096,
  });

  await expect(page.getByRole("progressbar")).toHaveAttribute("aria-valuenow", "25");
});
```

## CI

Spec runs on the Linux job in `.github/workflows/ci.yml`. macOS is
skipped today (Chromium-on-macOS adds CI time without catching
anything Linux misses for these mocked tests). The eventual
tauri-driver path (#57) will need its own macOS coverage.

## When mocked-Tauri tests are wrong

If a test fails *only* because the mocked invoke shape disagrees with
the real Rust command's serialised output, the bug is in the test
fixtures, not the app. Run the app via `cargo tauri dev` and inspect
the actual JSON payload on the wire (browser dev tools → Network →
WS), then update the default in `_mock.ts`. This is rare —
`#[serde(rename_all = "camelCase")]` is consistent across the
codebase — but worth knowing about.
