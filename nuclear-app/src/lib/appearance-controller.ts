import { normalizeAppError } from './frontend-errors';
import { readTheme, saveTheme, type ThemePreference } from './theme';

export function createAppearanceState() {
  return {
    theme: 'light' as ThemePreference,
    systemDark: false,
    saving: true,
    error: null as string | null
  };
}

export class AppearanceController {
  constructor(
    readonly state: ReturnType<typeof createAppearanceState>,
    private readonly isActive: () => boolean
  ) {}

  start(): () => void {
    const media = window.matchMedia?.('(prefers-color-scheme: dark)');
    this.state.systemDark = media?.matches ?? false;
    const onChange = (event: MediaQueryListEvent) => {
      this.state.systemDark = event.matches;
    };
    media?.addEventListener('change', onChange);
    void readTheme()
      .then((theme) => {
        if (this.isActive()) this.state.theme = theme;
      })
      .catch((error) => {
        if (this.isActive()) this.state.error = normalizeAppError(error);
      })
      .finally(() => {
        if (this.isActive()) this.state.saving = false;
      });
    return () => media?.removeEventListener('change', onChange);
  }

  async change(next: ThemePreference): Promise<void> {
    if (this.state.saving) return;
    const previous = this.state.theme;
    this.state.theme = next;
    this.state.saving = true;
    this.state.error = null;
    try {
      await saveTheme(next);
    } catch (error) {
      if (!this.isActive()) return;
      this.state.theme = previous;
      this.state.error = normalizeAppError(error);
    } finally {
      if (this.isActive()) this.state.saving = false;
    }
  }
}
