import { describe, expect, it } from 'vitest';
import {
  createStartupSubsystems,
  deriveStartupState,
  runtimeAllowsDownloads,
  runtimeStartupSubsystemState,
  type StartupSubsystems
} from './startup-state';

function ready(): StartupSubsystems {
  return {
    listeners: 'ready',
    runtime: 'ready',
    outputDirectory: 'ready',
    appVersion: 'ready',
    updateCheck: 'ready'
  };
}

describe('startup state', () => {
  it('is loading until independent initialization has settled', () => {
    expect(deriveStartupState(createStartupSubsystems())).toBe('loading');
  });

  it('degrades for recoverable subsystem failures', () => {
    expect(deriveStartupState({ ...ready(), outputDirectory: 'error' })).toBe('degraded');
    expect(deriveStartupState({ ...ready(), updateCheck: 'degraded' })).toBe('degraded');
  });

  it('fails closed when event delivery cannot be established', () => {
    expect(deriveStartupState({ ...ready(), listeners: 'error' })).toBe('error');
  });

  it('allows work only when local and authoritative runtime states are usable', () => {
    expect(runtimeAllowsDownloads('ready', 'ready')).toBe(true);
    expect(runtimeAllowsDownloads('ready_with_warnings', 'ready_with_warnings')).toBe(true);
    expect(runtimeAllowsDownloads('ready', 'update_available')).toBe(true);
    expect(runtimeAllowsDownloads('repair_required', 'ready')).toBe(false);
    expect(runtimeAllowsDownloads('ready', 'repair_required')).toBe(false);
    expect(runtimeAllowsDownloads('missing', 'ready')).toBe(false);
    expect(runtimeAllowsDownloads('ready', null)).toBe(false);
  });

  it('clears runtime startup degradation after a successful manual recheck', () => {
    expect(runtimeStartupSubsystemState('repair_required')).toBe('degraded');
    expect(runtimeStartupSubsystemState('ready_with_warnings')).toBe('degraded');
    expect(runtimeStartupSubsystemState('ready')).toBe('ready');
    expect(runtimeStartupSubsystemState(null)).toBe('error');
  });
});
