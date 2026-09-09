// @vitest-environment jsdom

import { cleanup, fireEvent, render, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { AppSnapshot } from '$lib/bindings/AppSnapshot';
import type { QueueItemRecord } from '$lib/bindings/QueueItemRecord';
import type { StateDelta } from '$lib/bindings/StateDelta';
import Page from './+page.svelte';

const ipc = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn() }));

vi.mock('$lib/ipc-client', () => ({ invokeCommand: ipc.invoke, listenEvent: ipc.listen }));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn(async () => '0.6.0') }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: vi.fn(), save: vi.fn() }));

const handlers = new Map<string, (event: { payload: StateDelta }) => void>();
const resizeObservers: ResizeObserverMock[] = [];

class ResizeObserverMock {
  readonly observe = vi.fn();
  readonly disconnect = vi.fn();

  constructor(readonly callback: (entries: Array<{ contentRect: { height: number } }>) => void) {
    resizeObservers.push(this);
  }
}

function validId(index: number): string {
  return `00000000-0000-4000-8000-${index.toString().padStart(12, '0')}`;
}

function record(index: number): QueueItemRecord {
  return {
    schemaVersion: 1,
    id: validId(index),
    sourceUrl: `https://example.test/video/${index}`,
    title: `Queue item ${index}`,
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
    createdAtMs: index,
    updatedAtMs: index
  };
}

function snapshot(size = 1000): AppSnapshot {
  return {
    schemaVersion: 1,
    queue: Array.from({ length: size }, (_, index) => record(index)),
    operations: [],
    runtimeReadiness: 'ready',
    maintenanceActive: false,
    draining: false,
    persistenceHealth: { degraded: false, error: null },
    latestSequence: 0
  };
}

function commandResult(command: string): unknown {
  switch (command) {
    case 'get_app_snapshot':
      return snapshot();
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
    case 'enqueue_queue_items':
      return [];
    default:
      return undefined;
  }
}

async function mountReadyPage() {
  const view = render(Page);
  await waitFor(() =>
    expect((view.getByRole('button', { name: 'Download All' }) as HTMLButtonElement).disabled).toBe(
      false
    )
  );
  return view;
}

async function scrollQueue(
  view: Awaited<ReturnType<typeof mountReadyPage>>,
  index: number
): Promise<HTMLElement> {
  const viewport = view.container.querySelector<HTMLElement>('section.queue')!;
  Object.defineProperty(viewport, 'scrollTop', { configurable: true, writable: true, value: 0 });
  viewport.scrollTop = index * 53;
  await fireEvent.scroll(viewport);
  await waitFor(() => expect(view.queryByLabelText(`Select Queue item ${index}`)).not.toBeNull());
  return viewport;
}

beforeEach(() => {
  vi.clearAllMocks();
  handlers.clear();
  resizeObservers.length = 0;
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

describe('mounted page queue component boundary', () => {
  it('bounds rendered rows and downloads a selected virtual row by its stable id', async () => {
    const view = await mountReadyPage();
    expect(view.container.querySelectorAll('tr.queue-item').length).toBeLessThan(40);

    await scrollQueue(view, 800);
    const checkbox = view.getByLabelText('Select Queue item 800');
    await fireEvent.click(checkbox);
    await fireEvent.click(view.getByRole('button', { name: 'Download Selected' }));

    await waitFor(() =>
      expect(ipc.invoke).toHaveBeenCalledWith('enqueue_queue_items', {
        itemIds: [validId(800)],
        priority: 'normal'
      })
    );
    expect(view.container.querySelectorAll('tr.queue-item').length).toBeLessThan(40);
  });

  it('keeps focus while editing a visible virtual row and commits Enter to its id', async () => {
    const view = await mountReadyPage();
    await scrollQueue(view, 640);
    await fireEvent.click(view.getByRole('button', { name: 'Queue item 640' }));
    const editor = await view.findByLabelText('Edit queued filename');
    await waitFor(() => expect(document.activeElement).toBe(editor));
    await fireEvent.input(editor, { target: { value: 'Renamed virtual item' } });
    await fireEvent.keyDown(editor, { key: 'Enter' });

    await waitFor(() =>
      expect(ipc.invoke).toHaveBeenCalledWith('update_queue_item', {
        itemId: validId(640),
        input: { filenameOverride: 'Renamed virtual item' }
      })
    );
  });

  it('observes the viewport and clamps scroll after the snapshot queue shrinks', async () => {
    const view = await mountReadyPage();
    const viewport = await scrollQueue(view, 900);
    expect(resizeObservers).toHaveLength(1);
    expect(resizeObservers[0].observe).toHaveBeenCalledOnce();
    expect(resizeObservers[0].observe).toHaveBeenCalledWith(viewport);
    resizeObservers[0].callback([{ contentRect: { height: 424 } }]);

    handlers.get('app-state-changed')!({
      payload: {
        schemaVersion: 1,
        sequence: 1,
        emittedAtMs: 1,
        kind: 'queue_items_removed',
        value: Array.from({ length: 995 }, (_, index) => validId(index + 5))
      }
    });

    await waitFor(() => expect(viewport.dataset.queueCount).toBe('5'));
    await waitFor(() => expect(viewport.scrollTop).toBe(0));
    expect(resizeObservers[0].disconnect).not.toHaveBeenCalled();
  });

  it('sets select-all indeterminate through row selection UI', async () => {
    const view = await mountReadyPage();
    const selectAll = view.getByLabelText('Select all queue items') as HTMLInputElement;
    expect(selectAll.indeterminate).toBe(false);
    await fireEvent.click(view.getByLabelText('Select Queue item 0'));
    await waitFor(() => expect(selectAll.indeterminate).toBe(true));
    await fireEvent.click(selectAll);
    await waitFor(() => expect(selectAll.indeterminate).toBe(false));
    expect(selectAll.checked).toBe(true);
  });
});
