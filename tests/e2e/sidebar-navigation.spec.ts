import { expect, test } from "@playwright/test";
import { installMocks } from "./_mock";

// Sidebar navigation coverage for sidebar-nav-toggle and sidebar-nav-dictation.
//
// The sidebar renders four nav items (dictation, history, settings, about)
// plus a collapse/expand toggle. The `sidebar-nav-{id}` items are used by
// many other spec files, but the toggle and dictation item had no dedicated
// assertions.
//
// sidebar-nav-history/settings/about are exercised indirectly in other files
// (e.g. gotoSection("history"), `sidebar-nav-settings` clicks in
// settings-window.spec.ts). These specs cover the two remaining gaps.

test.describe("sidebar navigation", () => {
  test("dictation item is active by default (sidebar-nav-dictation)", async ({
    page,
  }) => {
    // Dictation is the default section; aria-current="page" should reflect
    // the active item without any additional interaction.
    await installMocks(page);
    await page.goto("/");

    const btn = page.locator('[data-testid="sidebar-nav-dictation"]');
    await expect(btn).toBeVisible();
    await expect(btn).toHaveAttribute("aria-current", "page");
  });

  test("clicking a non-active item marks it active and deactivates dictation", async ({
    page,
  }) => {
    await installMocks(page);
    await page.goto("/");

    // Start on dictation.
    await expect(
      page.locator('[data-testid="sidebar-nav-dictation"]'),
    ).toHaveAttribute("aria-current", "page");

    // Click history — it should become active.
    await page.locator('[data-testid="sidebar-nav-history"]').click();
    await expect(
      page.locator('[data-testid="sidebar-nav-history"]'),
    ).toHaveAttribute("aria-current", "page");

    // Dictation should no longer be marked active.
    await expect(
      page.locator('[data-testid="sidebar-nav-dictation"]'),
    ).not.toHaveAttribute("aria-current", "page");
  });

  test("sidebar-nav-toggle collapses the sidebar and flips aria-expanded", async ({
    page,
  }) => {
    // Default: sidebarOpen = true (localStorage is empty → defaults to open).
    // aria-expanded="true" on the toggle button; labels render next to icons.
    await installMocks(page);
    await page.goto("/");

    const toggle = page.locator('[data-testid="sidebar-nav-toggle"]');
    await expect(toggle).toBeVisible();
    await expect(toggle).toHaveAttribute("aria-expanded", "true");

    // Collapse.
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-expanded", "false");

    // Expand again.
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-expanded", "true");
  });
});
