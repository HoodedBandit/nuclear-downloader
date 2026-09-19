import { invokeCommand } from './ipc-client';
export type ThemePreference = 'light' | 'dark' | 'system';
export function resolvedTheme(preference: ThemePreference, systemDark: boolean): 'light' | 'dark' {
  return preference === 'system' ? (systemDark ? 'dark' : 'light') : preference;
}
export function applyTheme(preference: ThemePreference, systemDark: boolean): void {
  document.documentElement.dataset.theme = resolvedTheme(preference, systemDark);
}
export async function readTheme(): Promise<ThemePreference> {
  const value = await invokeCommand('get_ui_theme');
  return value === 'dark' || value === 'system' ? value : 'light';
}
export async function saveTheme(preference: ThemePreference): Promise<void> {
  await invokeCommand('set_ui_theme', { theme: preference });
}
