import { fireEvent, render, waitFor } from '@testing-library/svelte';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import Page from './+page.svelte';

const ipc = vi.hoisted(() => ({
  invoke: vi.fn(),
  listen: vi.fn()
}));

vi.mock('$lib/ipc-client', () => ({
  invokeCommand: ipc.invoke,
  listenEvent: ipc.listen
}));
vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn(async () => '0.6.0') }));
vi.mock('@tauri-apps/plugin-dialog', () => ({
  open: vi.fn(),
  save: vi.fn()
}));

const eventNames = [
  'app-state-changed',
  'app-state-resync-required',
  'download-progress',
  'update-install-progress',
  'downloader-runtime-update-progress'
] as const;

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
    default:
      return undefined;
  }
}

class ResizeObserverMock {
  observe = vi.fn();
  disconnect = vi.fn();
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.stubGlobal('ResizeObserver', ResizeObserverMock);
  vi.stubGlobal('reportError', vi.fn());
  ipc.invoke.mockImplementation(async (command: string) => commandResult(command));
  ipc.listen.mockImplementation(async () => vi.fn());
});

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

describe('page lifecycle ownership', () => {
  it.each(eventNames)(
    'releases a late %s listener and stops startup after unmount',
    async (heldEvent) => {
      const unlisteners = new Map(eventNames.map((event) => [event, vi.fn()]));
      let resolveHeld!: (unlisten: () => void) => void;
      ipc.listen.mockImplementation((event: (typeof eventNames)[number]) => {
        if (event === heldEvent) {
          return new Promise((resolve) => {
            resolveHeld = resolve;
          });
        }
        return Promise.resolve(unlisteners.get(event));
      });

      const view = render(Page);
      await waitFor(() => expect(resolveHeld).toBeTypeOf('function'));
      view.unmount();
      resolveHeld(unlisteners.get(heldEvent)!);
      await waitFor(() => expect(unlisteners.get(heldEvent)).toHaveBeenCalledOnce());

      for (const event of eventNames) {
        const callIndex = eventNames.indexOf(event);
        const heldIndex = eventNames.indexOf(heldEvent);
        expect(ipc.listen.mock.calls.some(([name]) => name === event)).toBe(callIndex <= heldIndex);
        expect(unlisteners.get(event)).toHaveBeenCalledTimes(callIndex <= heldIndex ? 1 : 0);
      }
      expect(
        ipc.invoke.mock.calls.some(([command]) =>
          ['default_download_dir', 'check_downloader_runtime', 'check_app_update'].includes(command)
        )
      ).toBe(false);
      expect(ipc.invoke.mock.calls.some(([command]) => command === 'cancel_operation')).toBe(false);
      expect(ipc.invoke.mock.calls.some(([command]) => command === 'dismiss_operation')).toBe(
        false
      );
    }
  );

  it('owns a fresh listener set for every mount and releases each set once', async () => {
    const unlisteners: Array<ReturnType<typeof vi.fn>> = [];
    ipc.listen.mockImplementation(async () => {
      const unlisten = vi.fn();
      unlisteners.push(unlisten);
      return unlisten;
    });

    for (let mount = 0; mount < 2; mount += 1) {
      const view = render(Page);
      await waitFor(() =>
        expect(ipc.listen).toHaveBeenCalledTimes((mount + 1) * eventNames.length)
      );
      view.unmount();
    }

    expect(unlisteners).toHaveLength(eventNames.length * 2);
    for (const unlisten of unlisteners) expect(unlisten).toHaveBeenCalledOnce();
  });

  it('does not publish delayed startup results after unmount', async () => {
    let resolveSnapshot!: (value: typeof snapshot) => void;
    ipc.invoke.mockImplementation((command: string) => {
      if (command === 'get_app_snapshot') {
        return new Promise((resolve) => {
          resolveSnapshot = resolve;
        });
      }
      return Promise.resolve(commandResult(command));
    });

    const view = render(Page);
    await waitFor(() => expect(resolveSnapshot).toBeTypeOf('function'));
    view.unmount();
    resolveSnapshot(snapshot);
    await Promise.resolve();

    expect(ipc.invoke).toHaveBeenCalledTimes(1);
  });

  it.each(['default_download_dir', 'check_downloader_runtime', 'check_app_update'])(
    'does not continue a delayed %s initializer after unmount',
    async (heldCommand) => {
      let resolveInitializer!: (value: unknown) => void;
      ipc.invoke.mockImplementation((command: string) => {
        if (command === heldCommand) {
          return new Promise((resolve) => {
            resolveInitializer = resolve;
          });
        }
        return Promise.resolve(commandResult(command));
      });

      const view = render(Page);
      await waitFor(() => expect(resolveInitializer).toBeTypeOf('function'));
      view.unmount();
      const callsAtUnmount = ipc.invoke.mock.calls.length;
      resolveInitializer(commandResult(heldCommand));
      await Promise.resolve();
      await Promise.resolve();

      expect(ipc.invoke).toHaveBeenCalledTimes(callsAtUnmount);
      expect(ipc.invoke.mock.calls.some(([command]) => command === 'cancel_operation')).toBe(false);
      expect(ipc.invoke.mock.calls.some(([command]) => command === 'dismiss_operation')).toBe(
        false
      );
    }
  );

  it('does not admit or dismiss an inspection that completes after unmount', async () => {
    let stateHandler!: (event: { payload: unknown }) => void;
    ipc.listen.mockImplementation(async (event: string, handler: typeof stateHandler) => {
      if (event === 'app-state-changed') stateHandler = handler;
      return vi.fn();
    });
    ipc.invoke.mockImplementation(async (command: string) => {
      if (command === 'begin_inspection') return { operationId: 'inspection-1' };
      return commandResult(command);
    });

    const view = render(Page);
    const input = await view.findByLabelText('Video or playlist URL');
    const addButton = view.getByRole('button', { name: 'Add' });
    await waitFor(() =>
      expect(ipc.invoke.mock.calls.some(([command]) => command === 'check_app_update')).toBe(true)
    );
    await waitFor(() => expect((addButton as HTMLButtonElement).disabled).toBe(false));
    await fireEvent.input(input, {
      target: { value: 'https://www.youtube.com/watch?v=lifecycle-test' }
    });
    await fireEvent.submit(input.closest('form')!);
    await waitFor(() =>
      expect(ipc.invoke.mock.calls.some(([command]) => command === 'begin_inspection')).toBe(true)
    );
    view.unmount();

    stateHandler({
      payload: {
        schemaVersion: 1,
        sequence: 1,
        emittedAtMs: 1,
        kind: 'operation_upserted',
        value: {
          id: 'inspection-1',
          state: 'completed',
          inspectionResult: { kind: 'video', video: { url: 'https://example.com/video' } }
        }
      }
    });
    await Promise.resolve();

    expect(
      ipc.invoke.mock.calls.some(([command]) => command === 'add_inspection_result_to_queue')
    ).toBe(false);
    expect(ipc.invoke.mock.calls.some(([command]) => command === 'dismiss_operation')).toBe(false);
    expect(ipc.invoke.mock.calls.some(([command]) => command === 'cancel_operation')).toBe(false);
  });
});
