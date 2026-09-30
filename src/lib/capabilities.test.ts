import { readFileSync, readdirSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { describe, expect, it } from "vitest";

// Every Tauri *plugin* command the main window calls needs a grant in
// `src-tauri/capabilities/default.json`; custom `#[tauri::command]`s don't.
// A missing grant is an ACL rejection at runtime that e2e can't see, because
// Playwright mocks the plugin. That's how "Launch Hush at login" broke
// silently when Settings moved inline (#480) and its grants were dropped.
//
// The HUD, menu-bar and debug windows have their own capability files; their
// routes are excluded here and currently import no plugins.

const ROOT = join(__dirname, "..", "..");
const SRC = join(ROOT, "src");
const OTHER_WINDOW_ROUTES = ["routes/hud", "routes/menu-bar", "routes/debug"];

// Plugins whose frontend API reads injected globals rather than invoking an
// ACL-checked command, so they need no grant.
const NO_GRANT_NEEDED = new Set(["os"]);

function sourceFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return sourceFiles(path);
    return /\.(ts|svelte)$/.test(name) && !name.endsWith(".test.ts") ? [path] : [];
  });
}

function mainWindowPlugins(): Map<string, string[]> {
  const used = new Map<string, string[]>();
  for (const file of sourceFiles(SRC)) {
    const rel = relative(SRC, file);
    if (OTHER_WINDOW_ROUTES.some((r) => rel.startsWith(r))) continue;
    for (const m of readFileSync(file, "utf8").matchAll(/from "@tauri-apps\/plugin-([a-z-]+)"/g)) {
      used.set(m[1], [...(used.get(m[1]) ?? []), rel]);
    }
  }
  return used;
}

describe("main-window capability grants", () => {
  const permissions: string[] = JSON.parse(
    readFileSync(join(ROOT, "src-tauri/capabilities/default.json"), "utf8"),
  ).permissions;

  it("finds the plugins it is meant to guard", () => {
    expect(mainWindowPlugins().has("autostart")).toBe(true);
  });

  it("grants every plugin the main window imports", () => {
    const missing = [...mainWindowPlugins()]
      .filter(([plugin]) => !NO_GRANT_NEEDED.has(plugin))
      .filter(([plugin]) => !permissions.some((p) => p.startsWith(`${plugin}:`)))
      .map(([plugin, files]) => `${plugin} (imported by ${files.join(", ")})`);
    expect(missing, "add grants to src-tauri/capabilities/default.json").toEqual([]);
  });

  it("grants each autostart command the Launch-at-login toggle calls", () => {
    for (const cmd of ["is-enabled", "enable", "disable"]) {
      expect(permissions).toContain(`autostart:allow-${cmd}`);
    }
  });
});
