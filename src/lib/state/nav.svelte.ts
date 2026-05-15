import {
  readDebugConsoleEnabled,
  setDebugConsoleEnabled as persistDebugConsoleEnabled,
} from "$lib/debug-console";
import type { SettingsTab } from "$lib/settings-tabs";
import type { SidebarSection } from "$lib/SidebarNav.svelte";

const SIDEBAR_OPEN_KEY = "hush.sidebar.open";

let activeSection = $state<SidebarSection>("dictation");
let settingsActiveTab = $state<SettingsTab>("general");
let sidebarOpen = $state<boolean>(
  typeof localStorage !== "undefined"
    ? localStorage.getItem(SIDEBAR_OPEN_KEY) !== "false"
    : true,
);
let debugConsoleEnabled = $state<boolean>(readDebugConsoleEnabled());

export const nav = {
  get activeSection() {
    return activeSection;
  },
  set activeSection(val: SidebarSection) {
    activeSection = val;
  },
  get settingsActiveTab() {
    return settingsActiveTab;
  },
  set settingsActiveTab(val: SettingsTab) {
    settingsActiveTab = val;
  },
  get sidebarOpen() {
    return sidebarOpen;
  },
  set sidebarOpen(val: boolean) {
    sidebarOpen = val;
  },
  get debugConsoleEnabled() {
    return debugConsoleEnabled;
  },
  onSidebarSelect(id: SidebarSection) {
    activeSection = id;
    // Settings accordion requires the sidebar to be expanded (labels need
    // ~180 px). Force-expand without persisting so the user's collapsed
    // preference survives the next launch.
    if (id === "settings") sidebarOpen = true;
  },
  onSidebarToggle() {
    sidebarOpen = !sidebarOpen;
    try {
      localStorage.setItem(SIDEBAR_OPEN_KEY, sidebarOpen ? "true" : "false");
    } catch (e) {
      // localStorage write failure is non-fatal — the toggle still
      // flipped in-memory and the user's current session works
      // normally. Next launch reverts to the default if persistence
      // can't write (private mode, quota, etc.).
      console.warn("[hush] failed to persist sidebar.open", e);
    }
  },
  openSettingsTab(tab: SettingsTab | "about") {
    if (tab === "about") {
      activeSection = "about";
      return;
    }
    settingsActiveTab = tab;
    activeSection = "settings";
    // Force-expand sidebar so the settings accordion is reachable from
    // every programmatic entry point (command palette, banners, menus).
    sidebarOpen = true;
  },
  setDebugConsoleEnabled(enabled: boolean) {
    persistDebugConsoleEnabled(enabled);
    debugConsoleEnabled = enabled;
    if (!enabled && settingsActiveTab === "debug") {
      settingsActiveTab = "general";
    }
  },
};
