// Theme switching removed — app uses a single fixed palette.
export type ThemePref = "system";

export function readStoredTheme(): ThemePref { return "system"; }
export function applyThemeAttribute(_pref: ThemePref): void {}
export async function setTheme(_pref: ThemePref): Promise<void> {}
export function listenForThemeChanges(_onChange: (_: ThemePref) => void): Promise<() => void> {
  return Promise.resolve(() => {});
}
