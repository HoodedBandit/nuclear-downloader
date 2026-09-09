// @vitest-environment jsdom

import { cleanup, fireEvent, render, waitFor, within } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { AppSnapshot } from '$lib/bindings/AppSnapshot';
import type { OperationSnapshot } from '$lib/bindings/OperationSnapshot';
import type { PlaylistEntry } from '$lib/bindings/PlaylistEntry';
import type { StateDelta } from '$lib/bindings/StateDelta';
import Page from './+page.svelte';

const ipc = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));

vi.mock('$lib/ipc-client', () => ({ invokeCommand: ipc.invoke, listenEvent: ipc.listen }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn(async () => '0.6.0') }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));

const inspectionId = '00000000-0000-4000-8000-000000000001';
const playlistEntries = Array.from({ length: 205 }, (_, index) => ({
  id: `entry-${index}`,
  title: `Playlist entry ${index}`,
  duration: index + 1,
  url: `https://example.test/playlist/${index}`,
  thumbnail: null
})) satisfies PlaylistEntry[];

const snapshot = {
  schemaVersion: 1,
  queue: [],
  operations: [],
  runtimeReadiness: 'ready',
  maintenanceActive: false,
  draining: false,
  persistenceHealth: { degraded: false, error: null },
  latestSequence: 0
} satisfies AppSnapshot;

const completedInspection = {
  schemaVersion: 1,
  id: inspectionId,
  queueItemId: null,
  kind: 'inspection',
  state: 'completed',
  progress: 100,
  phase: 'complete',
  sequence: 1,
  createdAtMs: 1,
  updatedAtMs: 2,
  finishedAtMs: 2,
  error: null,
  inspectionResult: {
    kind: 'playlist',
    playlist: {
      title: 'Large mounted playlist',
      channel: 'Test channel',
      entry_count: playlistEntries.length,
      truncated: false,
      entries: playlistEntries
    }
  },
  publishedOutput: null,
  intendedTerminalOutcome: null,
  correlationId: 'playlist-dialog-baseline'
} satisfies OperationSnapshot;

const handlers = new Map<string, (event: { payload: StateDelta }) => void>();

class ResizeObserverMock {
  observe(): void {}
  disconnect(): void {}
}

function commandResult(command: string): unknown {
  switch (command) {
    case 'get_app_snapshot':
      return snapshot;
    case 'default_download_dir':
    case 'validate_output_directory':
      return 'C:\\Downloads';
    case 'check_downloader_runtime':
      return {
        state: 'ready',
        runtimeVersion: 'test',
        source: 'test',
        updateAvailable: false,
        latestRuntimeVersion: null,
        runtimeDir: null,
        pluginDir: 'C:\\Plugins',
        message: null,
        tools: []
      };
    case 'check_runtime_update':
      return { updateAvailable: false, latestRuntimeVersion: null, message: null };
    case 'check_app_update':
      return {
        currentVersion: '0.6.0',
        hasUpdate: false,
        latestVersion: null,
        notes: null,
        publishedAt: null,
        installerName: null
      };
    case 'begin_inspection':
      return { operationId: inspectionId };
    default:
      return undefined;
  }
}

async function openPlaylist() {
  const view = render(Page);
  const input = view.getByLabelText('Video or playlist URL');
  const add = view.getByRole('button', { name: 'Add' });
  await waitFor(() => expect((add as HTMLButtonElement).disabled).toBe(false));
  input.focus();
  await fireEvent.input(input, { target: { value: 'https://example.test/large-playlist' } });
  await fireEvent.submit(input.closest('form')!);
  await waitFor(() =>
    expect(ipc.invoke).toHaveBeenCalledWith('begin_inspection', expect.anything())
  );
  handlers.get('app-state-changed')?.({
    payload: {
      schemaVersion: 1,
      sequence: 1,
      emittedAtMs: 2,
      kind: 'operation_upserted',
      value: completedInspection
    }
  });
  await view.findByRole('dialog', { name: 'Large mounted playlist' });
  return { view, input };
}

beforeEach(() => {
  vi.clearAllMocks();
  handlers.clear();
  vi.stubGlobal('ResizeObserver', ResizeObserverMock);
  vi.stubGlobal('reportError', vi.fn());
  ipc.invoke.mockImplementation(async (command: string) => commandResult(command));
  ipc.listen.mockImplementation(
    async (event: string, handler: (event: { payload: StateDelta }) => void) => {
      handlers.set(event, handler);
      return vi.fn();
    }
  );
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('mounted page playlist dialog baseline', () => {
  it('pages 205 entries in 100-row windows and preserves global-index selection', async () => {
    const { view } = await openPlaylist();
    const dialog = view.getByRole('dialog', { name: 'Large mounted playlist' });
    expect(dialog.querySelectorAll('.playlist-entry')).toHaveLength(100);
    expect((view.getByRole('button', { name: 'Previous' }) as HTMLButtonElement).disabled).toBe(
      true
    );

    await fireEvent.click(view.getByRole('button', { name: 'Next' }));
    expect((await view.findByText('Playlist entry 100')).isConnected).toBe(true);
    expect(dialog.querySelectorAll('.playlist-entry')).toHaveLength(100);
    const entry137 = view.getByLabelText(/Playlist entry 137/);
    await fireEvent.click(entry137);
    const selectAll = view.getByLabelText('Select All') as HTMLInputElement;
    await waitFor(() => expect(selectAll.indeterminate).toBe(true));
    expect(view.getByText('204 of 205 selected').isConnected).toBe(true);

    await fireEvent.click(view.getByRole('button', { name: 'Next' }));
    expect((await view.findByText('Playlist entry 204')).isConnected).toBe(true);
    expect(dialog.querySelectorAll('.playlist-entry')).toHaveLength(5);
    expect((view.getByRole('button', { name: 'Next' }) as HTMLButtonElement).disabled).toBe(true);
    await fireEvent.click(view.getByRole('button', { name: 'Previous' }));
    await waitFor(() =>
      expect((view.getByLabelText(/Playlist entry 137/) as HTMLInputElement).checked).toBe(false)
    );
    expect((view.getByLabelText(/Playlist entry 136/) as HTMLInputElement).checked).toBe(true);
  });

  it('closes on Escape, dismisses the inspection, and restores focus', async () => {
    const { view, input } = await openPlaylist();
    const dialog = view.getByRole('dialog', { name: 'Large mounted playlist' });
    await waitFor(() =>
      expect(document.activeElement).toBe(
        within(dialog).getByRole('button', { name: 'Close playlist picker' })
      )
    );
    await fireEvent.keyDown(dialog, { key: 'Escape' });
    await waitFor(() => expect(view.queryByRole('dialog')).toBeNull());
    expect(document.activeElement).toBe(input);
    expect(ipc.invoke).toHaveBeenCalledWith('dismiss_operation', { operationId: inspectionId });
  });
});
