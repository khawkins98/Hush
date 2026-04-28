/// Path B smoke (#57). Drives the real Hush binary through
/// tauri-driver to prove the WebView mounts, the sidebar renders,
/// and clicking a nav item updates `aria-current`. This is the
/// minimum-viable Path B test — proves the runner + driver +
/// build are wired correctly. Subsequent specs cover the flows
/// Path A can't (HUD lifecycle, real model download, hotkey
/// round-trip — all sketched in tests/e2e-tauri/README.md).

import { browser, expect, $ } from "@wdio/globals";

describe("path B — sidebar navigation", () => {
  it("mounts the main window and lands on the Dictation tab", async () => {
    // The real backend's `setup` hook needs a moment to:
    //   1. Open / migrate the SQLite database
    //   2. Resolve the models directory and any pre-loaded model
    //   3. Wire up plugins (clipboard, notification, etc.)
    //   4. Hand `AppState` to Tauri's manager
    // The frontend's `onMount` then races; we wait on the
    // sidebar's brand chip to be visible as a "renderer is up"
    // signal before exercising any other element.
    const brand = await $('aside[aria-label="Sidebar navigation"] .brand-chip');
    await brand.waitForDisplayed({ timeout: 15_000 });

    // Default tab is Dictation. Match by stable testid rather
    // than visible label so a future copy edit doesn't break the
    // test.
    const dictation = await $('[data-testid="sidebar-nav-dictation"]');
    await expect(dictation).toHaveAttribute("aria-current", "page");
  });

  it("clicking a tab updates aria-current", async () => {
    const meetings = await $('[data-testid="sidebar-nav-meetings"]');
    await meetings.click();
    await expect(meetings).toHaveAttribute("aria-current", "page");

    const history = await $('[data-testid="sidebar-nav-history"]');
    await history.click();
    await expect(history).toHaveAttribute("aria-current", "page");
  });

  it("the History tab shows the empty-state copy on a fresh DB", async () => {
    // The Path A version of this assertion runs against a mocked
    // `history_search => []` fixture; Path B exercises the real
    // SQLite repo. The pre-flight assumption is "the user starts
    // with no transcripts" — true on a fresh test profile, which
    // tauri-driver gives us by default (each session uses an
    // ephemeral app-data dir).
    const empty = await $(".empty-history");
    await empty.waitForDisplayed({ timeout: 5_000 });
    await expect(empty).toBeDisplayed();
  });
});
