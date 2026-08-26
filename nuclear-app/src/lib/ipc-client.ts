import { invoke as tauriInvoke } from '@tauri-apps/api/core';
import { listen as tauriListen, type Event, type UnlistenFn } from '@tauri-apps/api/event';
import type { AddQueueItemInput } from './bindings/AddQueueItemInput';
import type { AppSnapshot } from './bindings/AppSnapshot';
import type { BeginInspectionInput } from './bindings/BeginInspectionInput';
import type { BeginOperationResult } from './bindings/BeginOperationResult';
import type { CancelAllResult } from './bindings/CancelAllResult';
import type { DownloaderRuntimeStatus } from './bindings/DownloaderRuntimeStatus';
import type { DownloaderRuntimeUpdateCheck } from './bindings/DownloaderRuntimeUpdateCheck';
import type { DownloaderRuntimeUpdateProgress } from './bindings/DownloaderRuntimeUpdateProgress';
import type { DownloadProgress } from './bindings/DownloadProgress';
import type { QueueItemRecord } from './bindings/QueueItemRecord';
import type { QueuePriority } from './bindings/QueuePriority';
import type { StateDelta } from './bindings/StateDelta';
import type { UpdateCheckResult } from './bindings/UpdateCheckResult';
import type { UpdateInstallProgress } from './bindings/UpdateInstallProgress';
import type { UpdateQueueItemInput } from './bindings/UpdateQueueItemInput';

const WEBDRIVER_STARTUP_RELEASE = '__NUCLEAR_WEBDRIVER_RELEASE_STARTUP__';

function createWebdriverStartupGate(): Promise<void> {
  if (import.meta.env.MODE !== 'webdriver' || typeof window === 'undefined') {
    return Promise.resolve();
  }

  return new Promise<void>((resolve) => {
    Object.defineProperty(window, WEBDRIVER_STARTUP_RELEASE, {
      configurable: true,
      enumerable: false,
      value: () => {
        delete (window as unknown as Record<string, unknown>)[WEBDRIVER_STARTUP_RELEASE];
        resolve();
      }
    });
  });
}

// Browser-mode WebDriver installs its IPC interception after navigation. This
// compile-time-only gate keeps onMount initialization from outrunning those
// mocks. Production builds replace MODE and eliminate this entire branch.
const webdriverStartupGate = createWebdriverStartupGate();

interface CommandContract<Args, Result> {
  args: Args;
  result: Result;
}

export interface CommandMap {
  get_app_snapshot: CommandContract<undefined, AppSnapshot>;
  begin_inspection: CommandContract<{ input: BeginInspectionInput }, BeginOperationResult>;
  add_inspection_result_to_queue: CommandContract<{ input: AddQueueItemInput }, QueueItemRecord>;
  update_queue_item: CommandContract<{ itemId: string; input: UpdateQueueItemInput }, undefined>;
  remove_queue_items: CommandContract<{ itemIds: string[] }, undefined>;
  enqueue_queue_items: CommandContract<
    { itemIds: string[]; priority: QueuePriority },
    BeginOperationResult[]
  >;
  cancel_operation: CommandContract<{ operationId: string }, undefined>;
  dismiss_operation: CommandContract<{ operationId: string }, undefined>;
  default_download_dir: CommandContract<undefined, string>;
  validate_output_directory: CommandContract<{ path: string }, string>;
  cancel_all_downloads: CommandContract<undefined, CancelAllResult>;
  check_downloader_runtime: CommandContract<undefined, DownloaderRuntimeStatus>;
  check_runtime_update: CommandContract<undefined, DownloaderRuntimeUpdateCheck>;
  begin_runtime_update: CommandContract<undefined, BeginOperationResult>;
  check_app_update: CommandContract<undefined, UpdateCheckResult>;
  begin_app_update: CommandContract<{ expectedVersion: string }, BeginOperationResult>;
  export_diagnostics: CommandContract<{ destination: string }, undefined>;
  clear_diagnostics: CommandContract<undefined, undefined>;
}

export interface EventMap {
  'app-state-changed': StateDelta;
  'download-progress': Omit<DownloadProgress, 'status' | 'phase'> & {
    status:
      | 'fetching'
      | 'ready'
      | 'queued'
      | 'downloading'
      | 'postprocessing'
      | 'cancelling'
      | 'completed'
      | 'error'
      | 'cancelled';
    phase: 'download' | 'postprocess' | 'waiting_conversion' | 'conversion' | 'complete' | null;
  };
  'update-install-progress': Omit<UpdateInstallProgress, 'status'> & {
    status: 'downloading' | 'verifying' | 'launching' | 'error';
  };
  'downloader-runtime-update-progress': Omit<DownloaderRuntimeUpdateProgress, 'status'> & {
    status: 'checking' | 'downloading' | 'installing' | 'complete' | 'error';
  };
}

export type NuclearCommand = keyof CommandMap;
export type NuclearEvent = keyof EventMap;
type CommandArguments<K extends NuclearCommand> = CommandMap[K]['args'] extends undefined
  ? []
  : [args: CommandMap[K]['args']];

/** Centralizes the Tauri boundary so command/event names cannot drift freely. */
export async function invokeCommand<K extends NuclearCommand>(
  command: K,
  ...args: CommandArguments<K>
): Promise<CommandMap[K]['result']> {
  await webdriverStartupGate;
  return tauriInvoke<CommandMap[K]['result']>(command, args[0]);
}

export async function listenEvent<K extends NuclearEvent>(
  event: K,
  handler: (event: Event<EventMap[K]>) => void
): Promise<UnlistenFn> {
  await webdriverStartupGate;
  return tauriListen<EventMap[K]>(event, handler);
}
