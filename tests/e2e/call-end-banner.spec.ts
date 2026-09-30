import { expect, test } from "@playwright/test";
import { fireEvent, installMocks } from "./_mock";

// The main window's "Your call has likely ended" banner is scoped to one
// meeting session (mirroring the HUD prompt). Pre-fix it (a) appeared with
// no meeting running, (b) outlived a stop made elsewhere (HUD ■, auto-stop)
// so its Stop button acted on nothing, and (c) one dismissal suppressed it
// for every later meeting.

const CALL_END = { confidence: "high", signalSummary: "mic inactive + system audio quiet" };

// Backend truth for `meeting_active_session`, which the frontend re-reads
// after every session event. Mocks are serialized and can't close over test
// variables, so the page reads it through an exposed function.
let activeId: number | null = null;

async function setActive(
  page: Parameters<typeof fireEvent>[0],
  event: string,
  sessionId: number,
  next: number | null,
) {
  activeId = next;
  await fireEvent(page, event, { sessionId });
}

test.describe("call-end banner", () => {
  test.beforeEach(async ({ page }) => {
    activeId = null;
    await page.exposeFunction("__hushActiveId", () => activeId);
    await installMocks(page, {
      meeting_active_session: async () => ({
        active: await (window as unknown as { __hushActiveId: () => Promise<number | null> })
          .__hushActiveId(),
      }),
    });
    await page.goto("/");
    await expect(page.getByRole("button", { name: "Start recording" })).toBeVisible();
  });

  test("ignored when no meeting is active", async ({ page }) => {
    await fireEvent(page, "meeting:call-may-have-ended", CALL_END);
    await page.waitForTimeout(200);
    await expect(page.getByTestId("call-end-banner")).toHaveCount(0);
  });

  test("shown during a meeting and cleared when it stops elsewhere", async ({ page }) => {
    await setActive(page, "meeting:session-started", 1, 1);
    await expect(page.getByRole("button", { name: /Stop/ }).first()).toBeVisible();
    await fireEvent(page, "meeting:call-may-have-ended", CALL_END);
    await expect(page.getByTestId("call-end-banner")).toBeVisible();

    await setActive(page, "meeting:finalizing", 1, null);
    await expect(page.getByTestId("call-end-banner")).toHaveCount(0);
  });

  test("dismissal only suppresses the current meeting", async ({ page }) => {
    await setActive(page, "meeting:session-started", 1, 1);
    await expect(page.getByRole("button", { name: /Stop/ }).first()).toBeVisible();
    await fireEvent(page, "meeting:call-may-have-ended", CALL_END);
    await page.getByTestId("call-end-banner").getByRole("button", { name: "Dismiss" }).click();

    // Same session: stays suppressed.
    await fireEvent(page, "meeting:call-may-have-ended", CALL_END);
    await page.waitForTimeout(200);
    await expect(page.getByTestId("call-end-banner")).toHaveCount(0);

    // Next session: prompts again.
    await setActive(page, "meeting:session-ended", 1, null);
    await setActive(page, "meeting:session-started", 2, 2);
    await expect(page.getByRole("button", { name: /Stop/ }).first()).toBeVisible();
    await fireEvent(page, "meeting:call-may-have-ended", CALL_END);
    await expect(page.getByTestId("call-end-banner")).toBeVisible();
  });
});
