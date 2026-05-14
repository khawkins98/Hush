// Tests for the permissions state module's derived banner and status logic.
//
// The module is a singleton — $state vars persist across tests.
// Each test resets the relevant state via the exposed API (setters +
// mocked invoke responses for diagnose()/refreshHealth()) so test
// order doesn't matter.
//
// @tauri-apps/api/core is mocked at module level; individual tests
// configure the mock return values via vi.mocked(invoke).mockImplementation.

import { vi, describe, it, expect, beforeEach } from "vitest";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

import { invoke } from "@tauri-apps/api/core";
import { permissions } from "$lib/state/permissions.svelte";
import { nav } from "$lib/state/nav.svelte";
import type {
  MacosPermissionDiagnostic,
  PermissionHealthResponse,
} from "$lib/types";

// ── Fixtures ──────────────────────────────────────────────────────────────────

const DIAGNOSTIC_RESET_CAPABLE: MacosPermissionDiagnostic = {
  bundleId: "io.github.khawkins98.hush",
  microphoneHint: "",
  inputMonitoringHint: "",
  canReset: true,
  statuses: {
    microphone: "granted",
    screenRecording: "granted",
    inputMonitoring: "granted",
  },
};

const HEALTH_STALE_MIC: PermissionHealthResponse = {
  health: {
    microphone: "stale",
    screenRecording: "confirmed",
    inputMonitoring: "confirmed",
  },
};

const HEALTH_ALL_CONFIRMED: PermissionHealthResponse = {
  health: {
    microphone: "confirmed",
    screenRecording: "confirmed",
    inputMonitoring: "confirmed",
  },
};

// Reset helper — brings the module back to a known baseline before each test.
async function resetState() {
  // Reset diagnostic to null (mock returns null → diagnostic = null)
  vi.mocked(invoke).mockResolvedValue(null as unknown as MacosPermissionDiagnostic);
  await permissions.diagnose();

  // Reset permissionHealth via a response with all-confirmed health
  vi.mocked(invoke).mockResolvedValue(HEALTH_ALL_CONFIRMED as unknown as MacosPermissionDiagnostic);
  await permissions.refreshHealth();

  // Reset local state
  permissions.staleBannerDismissed = false;
  nav.activeSection = "dictation";
  nav.settingsActiveTab = "general";
}

// ── showStaleBanner derived logic ─────────────────────────────────────────────

describe("permissions.showStaleBanner", () => {
  beforeEach(async () => {
    await resetState();
  });

  it("is false when diagnostic is null (initial state)", () => {
    // After reset, diagnostic is null → macosCapable is false → anyPermsStale is false
    expect(permissions.showStaleBanner).toBe(false);
  });

  it("is false when macosCapable is false (non-macOS or canReset = false)", async () => {
    const nonCapable: MacosPermissionDiagnostic = {
      ...DIAGNOSTIC_RESET_CAPABLE,
      canReset: false,
    };
    vi.mocked(invoke).mockResolvedValue(nonCapable as unknown as MacosPermissionDiagnostic);
    await permissions.diagnose();

    vi.mocked(invoke).mockResolvedValue(HEALTH_STALE_MIC as unknown as MacosPermissionDiagnostic);
    await permissions.refreshHealth();

    expect(permissions.showStaleBanner).toBe(false);
  });

  it("is true when mic permission is stale and banner not dismissed", async () => {
    vi.mocked(invoke).mockResolvedValue(DIAGNOSTIC_RESET_CAPABLE as unknown as MacosPermissionDiagnostic);
    await permissions.diagnose();

    vi.mocked(invoke).mockResolvedValue(HEALTH_STALE_MIC as unknown as MacosPermissionDiagnostic);
    await permissions.refreshHealth();

    expect(permissions.showStaleBanner).toBe(true);
  });

  it("is false when banner has been dismissed", async () => {
    vi.mocked(invoke).mockResolvedValue(DIAGNOSTIC_RESET_CAPABLE as unknown as MacosPermissionDiagnostic);
    await permissions.diagnose();

    vi.mocked(invoke).mockResolvedValue(HEALTH_STALE_MIC as unknown as MacosPermissionDiagnostic);
    await permissions.refreshHealth();

    permissions.staleBannerDismissed = true;
    expect(permissions.showStaleBanner).toBe(false);
  });

  it("is false while user is on the Permissions settings tab", async () => {
    vi.mocked(invoke).mockResolvedValue(DIAGNOSTIC_RESET_CAPABLE as unknown as MacosPermissionDiagnostic);
    await permissions.diagnose();

    vi.mocked(invoke).mockResolvedValue(HEALTH_STALE_MIC as unknown as MacosPermissionDiagnostic);
    await permissions.refreshHealth();

    // Navigate to the Permissions tab — banner should suppress
    nav.activeSection = "settings";
    nav.settingsActiveTab = "permissions";
    expect(permissions.showStaleBanner).toBe(false);
  });

  it("is true when on the settings section but NOT the permissions tab", async () => {
    vi.mocked(invoke).mockResolvedValue(DIAGNOSTIC_RESET_CAPABLE as unknown as MacosPermissionDiagnostic);
    await permissions.diagnose();

    vi.mocked(invoke).mockResolvedValue(HEALTH_STALE_MIC as unknown as MacosPermissionDiagnostic);
    await permissions.refreshHealth();

    nav.activeSection = "settings";
    nav.settingsActiveTab = "general";
    expect(permissions.showStaleBanner).toBe(true);
  });
});

// ── anyPermsStale ─────────────────────────────────────────────────────────────

describe("permissions.anyPermsStale", () => {
  beforeEach(async () => {
    await resetState();
  });

  it("is false when diagnostic is null", () => {
    expect(permissions.anyPermsStale).toBe(false);
  });

  it("is false when health is all confirmed", async () => {
    vi.mocked(invoke).mockResolvedValue(DIAGNOSTIC_RESET_CAPABLE as unknown as MacosPermissionDiagnostic);
    await permissions.diagnose();

    vi.mocked(invoke).mockResolvedValue(HEALTH_ALL_CONFIRMED as unknown as MacosPermissionDiagnostic);
    await permissions.refreshHealth();

    expect(permissions.anyPermsStale).toBe(false);
  });

  it("is true when inputMonitoring is stale", async () => {
    vi.mocked(invoke).mockResolvedValue(DIAGNOSTIC_RESET_CAPABLE as unknown as MacosPermissionDiagnostic);
    await permissions.diagnose();

    const healthStaleIM: PermissionHealthResponse = {
      health: { microphone: "confirmed", screenRecording: "confirmed", inputMonitoring: "stale" },
    };
    vi.mocked(invoke).mockResolvedValue(healthStaleIM as unknown as MacosPermissionDiagnostic);
    await permissions.refreshHealth();

    expect(permissions.anyPermsStale).toBe(true);
  });
});

// ── allPermsGranted ───────────────────────────────────────────────────────────

describe("permissions.allPermsGranted", () => {
  beforeEach(async () => {
    await resetState();
  });

  it("is false when diagnostic is null", () => {
    expect(permissions.allPermsGranted).toBe(false);
  });

  it("is true when microphone is granted and inputMonitoring is not denied", async () => {
    vi.mocked(invoke).mockResolvedValue(DIAGNOSTIC_RESET_CAPABLE as unknown as MacosPermissionDiagnostic);
    await permissions.diagnose();
    expect(permissions.allPermsGranted).toBe(true);
  });

  it("is false when microphone is denied", async () => {
    const denied: MacosPermissionDiagnostic = {
      ...DIAGNOSTIC_RESET_CAPABLE,
      statuses: {
        microphone: "denied",
        screenRecording: "granted",
        inputMonitoring: "granted",
      },
    };
    vi.mocked(invoke).mockResolvedValue(denied as unknown as MacosPermissionDiagnostic);
    await permissions.diagnose();
    expect(permissions.allPermsGranted).toBe(false);
  });

  it("is false when inputMonitoring is denied", async () => {
    const denied: MacosPermissionDiagnostic = {
      ...DIAGNOSTIC_RESET_CAPABLE,
      statuses: {
        microphone: "granted",
        screenRecording: "granted",
        inputMonitoring: "denied",
      },
    };
    vi.mocked(invoke).mockResolvedValue(denied as unknown as MacosPermissionDiagnostic);
    await permissions.diagnose();
    expect(permissions.allPermsGranted).toBe(false);
  });
});
