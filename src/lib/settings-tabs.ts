// Settings tab metadata — shared between SidebarNav (accordion) and SettingsPanel (content switch).
// Extracted so neither component circularly imports the other for just this type.

export type SettingsTab =
  | "general"
  | "model"
  | "vocabulary"
  | "replacements"
  | "meeting"
  | "permissions"
  | "debug";

export const SETTINGS_TABS = [
  { key: "general"      as SettingsTab, label: "General",      testId: "settings-tab-general" },
  { key: "model"        as SettingsTab, label: "Model",        testId: "settings-tab-model" },
  { key: "vocabulary"   as SettingsTab, label: "Vocabulary",   testId: "settings-tab-vocabulary" },
  { key: "replacements" as SettingsTab, label: "Replacements", testId: "settings-tab-replacements" },
  { key: "meeting"      as SettingsTab, label: "Meeting",      testId: "settings-tab-meeting" },
  { key: "permissions"  as SettingsTab, label: "Permissions",  testId: "settings-tab-permissions" },
  { key: "debug"        as SettingsTab, label: "Debug",        testId: "settings-tab-debug" },
] satisfies Array<{ key: SettingsTab; label: string; testId: string }>;
