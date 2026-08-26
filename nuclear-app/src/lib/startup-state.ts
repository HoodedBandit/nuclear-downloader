import type { RuntimeReadiness } from './bindings/RuntimeReadiness';

export type StartupState = 'loading' | 'ready' | 'degraded' | 'error';
export type StartupSubsystem =
  'listeners' | 'runtime' | 'outputDirectory' | 'appVersion' | 'updateCheck';
export type StartupSubsystemState = 'loading' | 'ready' | 'degraded' | 'error';

export type StartupSubsystems = Record<StartupSubsystem, StartupSubsystemState>;

export function createStartupSubsystems(): StartupSubsystems {
  return {
    listeners: 'loading',
    runtime: 'loading',
    outputDirectory: 'loading',
    appVersion: 'loading',
    updateCheck: 'loading'
  };
}

export function deriveStartupState(parts: StartupSubsystems): StartupState {
  const values = Object.values(parts);
  if (values.some((value) => value === 'loading')) return 'loading';

  // Without event delivery the renderer cannot reconcile long-running work.
  if (parts.listeners === 'error') return 'error';

  return values.some((value) => value === 'error' || value === 'degraded') ? 'degraded' : 'ready';
}

export function runtimeStartupSubsystemState(localState: string | null): StartupSubsystemState {
  if (localState === null) return 'error';
  return localState === 'ready' ? 'ready' : 'degraded';
}

export function runtimeAllowsDownloads(
  localState: string | null,
  backendState: RuntimeReadiness | null
): boolean {
  const localReady = localState === 'ready' || localState === 'ready_with_warnings';
  const backendReady =
    backendState === 'ready' ||
    backendState === 'ready_with_warnings' ||
    backendState === 'update_available';
  return localReady && backendReady;
}
