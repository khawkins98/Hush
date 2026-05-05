import type { SettingsTab } from "$lib/SettingsPanel.svelte";
import type { SidebarSection } from "$lib/SidebarNav.svelte";

const SIDEBAR_OPEN_KEY = "hush.sidebar.open";

let activeSection = $state<SidebarSection>("dictation");
let settingsActiveTab = $state<SettingsTab>("general");
let sidebarOpen = $state<boolean>(
  typeof localStorage !== "undefined"
    ? localStorage.getItem(SIDEBAR_OPEN_KEY) !== "false"
    : true,
);

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
  onSidebarSelect(id: SidebarSection) {
    activeSection = id;
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
  },
};
