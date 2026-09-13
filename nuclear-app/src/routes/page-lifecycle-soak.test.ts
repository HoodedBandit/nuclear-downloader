import { cleanup, fireEvent, render, within } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { OperationSnapshot } from '$lib/bindings/OperationSnapshot';
import type { QueueItemRecord } from '$lib/bindings/QueueItemRecord';
import type { EventMap } from '$lib/ipc-client';
import Page from './+page.svelte';

const ipc = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));

vi.mock('$lib/ipc-client', () => ({ invokeCommand: ipc.invoke, listenEvent: ipc.listen }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn(async () => '0.6.0') }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));

const eventNames = [
  'app-state-changed',
  'app-state-resync-required',
  'download-progress',
  'update-install-progress',
  'downloader-runtime-update-progress'
] as const;
const soakSetting = process.env.NUCLEAR_RENDERER_SOAK_SECONDS;
const parsedSoakSeconds = Number(soakSetting);
const validSoakDuration = Number.isFinite(parsedSoakSeconds) && parsedSoakSeconds > 0;
const soakSeconds = validSoakDuration ? parsedSoakSeconds : 0;
const soakTest = soakSetting === undefined ? it.skip : it;
const operationTimeoutMs = 5 * 60 * 1000;
const operationRefreshMs = 1_000;

const snapshot = {
  schemaVersion: 1,
  queue: [],
  operations: [],
  runtimeReadiness: 'ready',
  maintenanceActive: false,
  draining: false,
  persistenceHealth: { degraded: false, error: null },
  latestSequence: 0
};

const admittedRecord = {
  schemaVersion: 1,
  id: 'completed-soak-item',
  sourceUrl: 'https://example.test/completed',
  title: 'Completed soak item',
  availableQualities: ['best', '1080p'],
  hasAudio: true,
  cookieConfig: null,
  format: 'mp4',
  quality: 'best',
  outputDir: 'C:\\Downloads',
  filenameOverride: null,
  compatConfigPath: null,
  state: 'inert',
  latestOperationId: null,
  createdAtMs: 2,
  updatedAtMs: 2
} satisfies QueueItemRecord;

const completedInspection = {
  schemaVersion: 1,
  id: 'soak-inspection',
  kind: 'inspection',
  queueItemId: null,
  state: 'completed',
  progress: 100,
  phase: 'complete',
  sequence: 1,
  createdAtMs: 1,
  updatedAtMs: 2,
  finishedAtMs: 2,
  error: null,
  inspectionResult: {
    kind: 'video',
    video: {
      id: 'completed-soak-item',
      url: admittedRecord.sourceUrl,
      title: admittedRecord.title,
      duration: null,
      channel: null,
      thumbnail: null,
      has_audio: true,
      available_qualities: ['1080p']
    }
  },
  publishedOutput: null,
  intendedTerminalOutcome: null,
  correlationId: 'soak-correlation'
} satisfies OperationSnapshot;

const playlistInspection = {
  ...completedInspection,
  // commandResult(begin_inspection) returns this operation ID; the page waiter
  // intentionally ignores terminal events for unrelated operations.
  id: 'soak-inspection',
  inspectionResult: {
    kind: 'playlist',
    playlist: {
      title: 'Lifecycle soak playlist',
      channel: 'Local fixture',
      entry_count: 2,
      truncated: false,
      entries: [0, 1].map((index) => ({
        id: `soak-playlist-${index}`,
        title: `Soak playlist row ${index}`,
        duration: null,
        url: `https://example.test/soak-playlist/${index}`,
        thumbnail: null,
        selection: {
          entryId: `soak-playlist-${index}`,
          extractorKey: 'Synthetic',
          playlistIndex: index + 1
        }
      }))
    }
  }
} satisfies OperationSnapshot;

const playlistRecords = [0, 1].map((index) => ({
  ...admittedRecord,
  id: `soak-playlist-row-${index}`,
  sourceUrl: `https://example.test/soak-playlist/${index}`,
  title: `Soak playlist row ${index}`,
  availableQualities: [],
  hasAudio: false,
  selection: {
    entryId: `soak-playlist-${index}`,
    extractorKey: 'Synthetic',
    playlistIndex: index + 1
  },
  preparation: 'pending',
  preparationOperationId: `soak-preparation-${index}`,
  latestOperationId: `soak-preparation-${index}`
})) satisfies QueueItemRecord[];

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
      return { operationId: 'soak-inspection' };
    case 'add_inspection_result_to_queue':
      return admittedRecord;
    default:
      return undefined;
  }
}

const resizeObservers: Array<{ observeCount: number; disconnectCount: number }> = [];

class ResizeObserverMock {
  observeCount = 0;
  disconnectCount = 0;

  constructor() {
    resizeObservers.push(this);
  }

  observe(): void {
    this.observeCount += 1;
  }

  disconnect(): void {
    this.disconnectCount += 1;
  }
}

async function until(predicate: () => boolean, label: string): Promise<void> {
  for (let attempt = 0; attempt < 100; attempt += 1) {
    if (predicate()) return;
    await new Promise<void>((resolve) => setTimeout(resolve, 0));
  }
  throw new Error(`Timed out waiting for ${label}`);
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.stubGlobal('ResizeObserver', ResizeObserverMock);
  vi.stubGlobal('reportError', vi.fn());
  resizeObservers.length = 0;
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
  document.body.replaceChildren();
});

describe('opt-in mounted page lifecycle soak', () => {
  soakTest(
    `releases resources through mixed mount/unmount races for ${soakSeconds} seconds`,
    async () => {
      const startedAt = performance.now();
      expect(
        validSoakDuration,
        'NUCLEAR_RENDERER_SOAK_SECONDS must be a finite number greater than zero'
      ).toBe(true);
      const deadline = startedAt + soakSeconds * 1000;
      const baselineHeap = process.memoryUsage().heapUsed;
      let maximumHeap = baselineHeap;
      const heapSamples: Array<{ elapsedMs: number; heapUsedBytes: number }> = [
        { elapsedMs: 0, heapUsedBytes: baselineHeap }
      ];
      let nextHeapSampleAt = startedAt + 60_000;
      let mounts = 0;
      let activeOperationCycles = 0;
      let lateListenerCycles = 0;
      let delayedStartupCycles = 0;
      let completedWorkflowCycles = 0;
      let playlistWorkflowCycles = 0;
      let playlistResyncCycles = 0;
      let currentOwnedTimerIds = new Set<unknown>();
      let currentClearedTimerIds = new Set<unknown>();
      const nativeSetTimeout = globalThis.setTimeout;
      const nativeClearTimeout = globalThis.clearTimeout;
      const setTimeoutSpy = vi.spyOn(globalThis, 'setTimeout').mockImplementation(((
        handler: TimerHandler,
        timeout?: number,
        ...args: unknown[]
      ) => {
        const id = nativeSetTimeout(handler, timeout, ...args);
        if (timeout === operationTimeoutMs || timeout === operationRefreshMs) {
          currentOwnedTimerIds.add(id);
        }
        return id;
      }) as typeof setTimeout);
      const clearTimeoutSpy = vi.spyOn(globalThis, 'clearTimeout').mockImplementation((id) => {
        if (id !== undefined && currentOwnedTimerIds.has(id)) currentClearedTimerIds.add(id);
        return nativeClearTimeout(id);
      });

      while (performance.now() < deadline || mounts < 4) {
        const mode = mounts % 5;
        resizeObservers.length = 0;
        const unlistenCounts = new Map(eventNames.map((name) => [name, 0]));
        let resolveLateListener: ((unlisten: () => void) => void) | undefined;
        let resolveSnapshot: ((value: typeof snapshot) => void) | undefined;
        let currentSnapshot = snapshot;
        const eventHandlers = new Map<keyof EventMap, (event: { payload: unknown }) => void>();
        const emit = <K extends keyof EventMap>(name: K, payload: EventMap[K]): void => {
          eventHandlers.get(name)?.({ payload });
        };
        currentOwnedTimerIds = new Set();
        currentClearedTimerIds = new Set();

        ipc.listen.mockImplementation(
          (event: (typeof eventNames)[number], handler: (event: { payload: unknown }) => void) => {
            eventHandlers.set(event, handler);
            const unlisten = () => {
              unlistenCounts.set(event, (unlistenCounts.get(event) ?? 0) + 1);
            };
            if (mode === 1 && event === 'download-progress') {
              return new Promise((resolve) => {
                resolveLateListener = resolve;
              });
            }
            return Promise.resolve(unlisten);
          }
        );
        ipc.invoke.mockImplementation((command: string, args?: unknown) => {
          if (mode === 2 && command === 'get_app_snapshot') {
            return new Promise((resolve) => {
              resolveSnapshot = resolve;
            });
          }
          if (command === 'get_app_snapshot') return Promise.resolve(currentSnapshot);
          if (
            command === 'add_inspection_result_to_queue' &&
            (args as { input?: { playlist?: unknown } } | undefined)?.input?.playlist
          ) {
            return Promise.resolve({
              kind: 'playlist',
              requestId: (args as { input: { playlist: { requestId: string } } }).input.playlist
                .requestId,
              itemIds: playlistRecords.map((record) => record.id),
              skippedCount: 0
            });
          }
          return Promise.resolve(commandResult(command));
        });

        const view = render(Page);
        if (mode === 0) {
          await until(
            () => ipc.invoke.mock.calls.some(([command]) => command === 'check_app_update'),
            'startup completion'
          );
          const input = view.getByLabelText('Video or playlist URL');
          const addButton = view.getByRole('button', { name: 'Add' });
          await until(() => !(addButton as HTMLButtonElement).disabled, 'enabled Add button');
          await fireEvent.input(input, { target: { value: 'https://example.test/soak' } });
          await fireEvent.submit(input.closest('form')!);
          await until(() => currentOwnedTimerIds.size === 2, 'owned operation waiter timers');
          activeOperationCycles += 1;
        } else if (mode === 1) {
          await until(() => resolveLateListener !== undefined, 'late listener subscription');
          lateListenerCycles += 1;
        } else if (mode === 2) {
          await until(() => resolveSnapshot !== undefined, 'delayed startup snapshot');
          delayedStartupCycles += 1;
        } else if (mode === 3) {
          await until(
            () => ipc.invoke.mock.calls.some(([command]) => command === 'check_app_update'),
            'startup completion'
          );
          const input = view.getByLabelText('Video or playlist URL');
          const addButton = view.getByRole('button', { name: 'Add' });
          await until(() => !(addButton as HTMLButtonElement).disabled, 'enabled Add button');
          await fireEvent.input(input, { target: { value: 'https://example.test/completed' } });
          await fireEvent.submit(input.closest('form')!);
          await until(() => currentOwnedTimerIds.size === 2, 'completed workflow waiter timers');
          emit('update-install-progress', {
            status: 'downloading',
            version: '0.6.1',
            downloadedBytes: 5,
            totalBytes: 10,
            message: 'soak app update'
          });
          emit('downloader-runtime-update-progress', {
            status: 'downloading',
            version: '2026.09.09',
            downloadedBytes: 5,
            totalBytes: 10,
            message: 'soak runtime update'
          });
          emit('app-state-changed', {
            schemaVersion: 1,
            sequence: 1,
            emittedAtMs: 1,
            kind: 'operation_upserted',
            value: completedInspection
          });
          await until(
            () =>
              ipc.invoke.mock.calls.some(
                ([command]) => command === 'add_inspection_result_to_queue'
              ),
            'inspection admission'
          );
          emit('app-state-changed', {
            schemaVersion: 1,
            sequence: 2,
            emittedAtMs: 2,
            kind: 'queue_item_upserted',
            value: admittedRecord
          });
          await until(
            () => view.queryByText(admittedRecord.title) !== null,
            'admitted queue row rendering'
          );
          completedWorkflowCycles += 1;
        } else {
          await until(
            () => ipc.invoke.mock.calls.some(([command]) => command === 'check_app_update'),
            'playlist startup completion'
          );
          const input = view.getByLabelText('Video or playlist URL');
          const addButton = view.getByRole('button', { name: 'Add' });
          await until(
            () => !(addButton as HTMLButtonElement).disabled,
            'enabled playlist Add button'
          );
          await fireEvent.input(input, { target: { value: 'https://example.test/soak-playlist' } });
          await fireEvent.submit(input.closest('form')!);
          await until(() => currentOwnedTimerIds.size === 2, 'playlist workflow waiter timers');
          emit('app-state-changed', {
            schemaVersion: 1,
            sequence: 1,
            emittedAtMs: 1,
            kind: 'operation_upserted',
            value: playlistInspection
          });
          await until(
            () => view.queryByRole('dialog', { name: 'Lifecycle soak playlist' }) !== null,
            'playlist dialog rendering'
          );
          const dialog = view.getByRole('dialog', { name: 'Lifecycle soak playlist' });
          await fireEvent.click(
            within(dialog).getByRole('button', { name: 'Add 2 Videos to Queue' })
          );
          await until(
            () =>
              ipc.invoke.mock.calls.some(
                ([command]) => command === 'add_inspection_result_to_queue'
              ),
            'playlist batch admission'
          );
          for (const [index, record] of playlistRecords.entries()) {
            emit('app-state-changed', {
              schemaVersion: 1,
              sequence: index + 2,
              emittedAtMs: index + 2,
              kind: 'queue_item_upserted',
              value: record
            });
          }
          await until(
            () => playlistRecords.every((record) => view.queryByText(record.title) !== null),
            'pending playlist rows rendering'
          );
          const snapshotsBeforeResync = ipc.invoke.mock.calls.filter(
            ([command]) => command === 'get_app_snapshot'
          ).length;
          currentSnapshot = { ...snapshot, latestSequence: 4 };
          emit('app-state-resync-required', { latestSequence: 4 });
          await until(
            () =>
              ipc.invoke.mock.calls.filter(([command]) => command === 'get_app_snapshot').length >
              snapshotsBeforeResync,
            'playlist resync snapshot'
          );
          await until(
            () => playlistRecords.every((record) => view.queryByText(record.title) === null),
            'playlist row disposal after resync'
          );
          playlistWorkflowCycles += 1;
          playlistResyncCycles += 1;
        }

        const commandsAtUnmount = ipc.invoke.mock.calls.length;
        view.unmount();
        resolveLateListener?.(() => {
          unlistenCounts.set(
            'download-progress',
            (unlistenCounts.get('download-progress') ?? 0) + 1
          );
        });
        resolveSnapshot?.(snapshot);
        await Promise.resolve();
        await Promise.resolve();

        for (const name of eventNames) {
          const subscribed = ipc.listen.mock.calls.some(([event]) => event === name);
          expect(unlistenCounts.get(name)).toBe(subscribed ? 1 : 0);
        }
        expect(currentClearedTimerIds.size).toBe(currentOwnedTimerIds.size);
        expect(ipc.invoke.mock.calls.slice(commandsAtUnmount)).toHaveLength(0);
        expect(resizeObservers).toHaveLength(1);
        expect(resizeObservers[0].disconnectCount).toBe(1);

        cleanup();
        vi.clearAllMocks();
        mounts += 1;
        const heapUsed = process.memoryUsage().heapUsed;
        maximumHeap = Math.max(maximumHeap, heapUsed);
        if (performance.now() >= nextHeapSampleAt) {
          heapSamples.push({
            elapsedMs: Math.round(performance.now() - startedAt),
            heapUsedBytes: heapUsed
          });
          nextHeapSampleAt += 60_000;
        }
      }

      const finalHeap = process.memoryUsage().heapUsed;
      expect(mounts).toBeGreaterThan(0);
      expect(activeOperationCycles).toBeGreaterThan(0);
      expect(lateListenerCycles).toBeGreaterThan(0);
      expect(delayedStartupCycles).toBeGreaterThan(0);
      expect(completedWorkflowCycles).toBeGreaterThan(0);
      expect(playlistWorkflowCycles).toBeGreaterThan(0);
      expect(playlistResyncCycles).toBe(playlistWorkflowCycles);
      setTimeoutSpy.mockRestore();
      clearTimeoutSpy.mockRestore();
      const exposedGc = (globalThis as typeof globalThis & { gc?: () => void }).gc;
      const gcAvailable = typeof exposedGc === 'function';
      exposedGc?.();
      const afterOptionalGcHeap = process.memoryUsage().heapUsed;
      console.info(
        `NUCLEAR_RENDERER_SOAK_RESULT=${JSON.stringify({
          provenance:
            'Vitest jsdom; mocked Tauri IPC; JS heap only; no native renderer or OS resources',
          configuredSeconds: soakSeconds,
          elapsedMs: Math.round(performance.now() - startedAt),
          mounts,
          activeOperationCycles,
          lateListenerCycles,
          delayedStartupCycles,
          completedWorkflowCycles,
          playlistWorkflowCycles,
          playlistResyncCycles,
          heapUsedBytes: {
            baseline: baselineHeap,
            maximum: maximumHeap,
            final: finalHeap,
            afterOptionalGc: afterOptionalGcHeap
          },
          heapSamples,
          gcAvailable,
          heapBoundedness:
            'observational only; no arbitrary pass threshold; postGc is controlled only when Node exposes global.gc'
        })}`
      );
    },
    Math.max(30_000, soakSeconds * 1000 + 30_000)
  );
});
