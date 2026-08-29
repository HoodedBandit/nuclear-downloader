<script lang="ts">
  import { getVersion } from '@tauri-apps/api/app';
  import { open, save } from '@tauri-apps/plugin-dialog';
  import { onMount, tick } from 'svelte';
  import { SvelteMap } from 'svelte/reactivity';
  import { accessibleDialog } from '$lib/accessible-dialog';
  import { AppStateController } from '$lib/app-state-controller';
  import { isTerminalOperation, latestOperationForItem } from '$lib/backend-state';
  import type { AppSnapshot } from '$lib/bindings/AppSnapshot';
  import type { CookieConfig as BackendCookieConfig } from '$lib/bindings/CookieConfig';
  import type { DownloaderRuntimeStatus } from '$lib/bindings/DownloaderRuntimeStatus';
  import type { DownloaderRuntimeUpdateCheck } from '$lib/bindings/DownloaderRuntimeUpdateCheck';
  import type { DownloaderToolStatus } from '$lib/bindings/DownloaderToolStatus';
  import type { OperationSnapshot } from '$lib/bindings/OperationSnapshot';
  import type { PlaylistEntry } from '$lib/bindings/PlaylistEntry';
  import type { PlaylistInfo } from '$lib/bindings/PlaylistInfo';
  import type { QueueItemRecord } from '$lib/bindings/QueueItemRecord';
  import type { StateDelta } from '$lib/bindings/StateDelta';
  import type { UpdateCheckResult } from '$lib/bindings/UpdateCheckResult';
  import type { UrlInspection } from '$lib/bindings/UrlInspection';
  import type { VideoInfo } from '$lib/bindings/VideoInfo';
  import { invokeCommand as invoke, listenEvent as listen, type EventMap } from '$lib/ipc-client';
  import { reduceOperationProgress } from '$lib/operation-reducer';
  import { OperationWaitRegistry } from '$lib/operation-wait-registry';
  import {
    canStartWork,
    deriveSelectionState,
    isAudioOnlyFormat,
    isUpdateBlockingStatus,
    redactDiagnosticText,
    resolveAvailableFormat,
    resolveAvailableQuality
  } from '$lib/queue-logic';
  import {
    createStartupSubsystems,
    deriveStartupState,
    runtimeAllowsDownloads,
    runtimeStartupSubsystemState,
    type StartupSubsystem,
    type StartupSubsystemState
  } from '$lib/startup-state';

  type DownloadStatus =
    | 'fetching'
    | 'ready'
    | 'queued'
    | 'downloading'
    | 'postprocessing'
    | 'cancelling'
    | 'completed'
    | 'error'
    | 'cancelled';

  type DownloadPhase =
    'download' | 'postprocess' | 'waiting_conversion' | 'conversion' | 'complete';
  type CookieMode = 'browser' | 'file';

  const supportedBrowsers = ['firefox', 'chrome', 'edge', 'brave', 'opera', 'chromium'] as const;
  type BrowserName = (typeof supportedBrowsers)[number];

  const videoFormats = ['mp4', 'mkv', 'webm'] as const;
  const audioFormats = ['mp3', 'flac', 'wav', 'aac', 'opus'] as const;
  type VideoFormat = (typeof videoFormats)[number];
  type AudioFormat = (typeof audioFormats)[number];
  type OutputFormat = VideoFormat | AudioFormat;
  const PLAYLIST_PAGE_SIZE = 100;
  const DOWNLOAD_DISPLAY_UPDATE_INTERVAL_MS = 500;
  const QUEUE_ROW_HEIGHT_PX = 53;
  const QUEUE_ROW_OVERSCAN = 8;
  const MAX_CUSTOM_FILENAME_UTF16_UNITS = 180;
  const WINDOWS_INVALID_FILENAME_CHARS = '<>:"/\\|?*';
  const WINDOWS_RESERVED_FILENAME_STEMS = new Set([
    'CON',
    'PRN',
    'AUX',
    'NUL',
    ...Array.from({ length: 9 }, (_, index) => `COM${index + 1}`),
    ...Array.from({ length: 9 }, (_, index) => `LPT${index + 1}`)
  ]);
  const OPERATION_PROJECTION_FIELDS = [
    'downloadId',
    'status',
    'progress',
    'downloadProgress',
    'conversionProgress',
    'phase',
    'speed',
    'eta',
    'error',
    'errorCode',
    'errorDetail'
  ] as const satisfies readonly (keyof QueueItem)[];

  type CookieConfig = Omit<BackendCookieConfig, 'mode' | 'browser'> & {
    mode: CookieMode;
    browser: BrowserName;
  };

  interface PlaylistModalEntry extends PlaylistEntry {
    selected: boolean;
  }

  interface PlaylistModal {
    info: PlaylistInfo;
    inspectionOperationId: string;
    url: string;
    cookieConfig: CookieConfig | null;
    entries: PlaylistModalEntry[];
  }

  interface CompletedInspection {
    operationId: string;
    inspection: UrlInspection;
  }

  interface QueueItem {
    id: string;
    downloadId: string | null;
    url: string;
    title: string;
    customFilename: string | null;
    duration: number | null;
    channel: string | null;
    thumbnail: string | null;
    infoLoaded: boolean;
    hasAudio: boolean | null;
    status: DownloadStatus;
    quality: string;
    format: OutputFormat;
    cookieConfig: CookieConfig | null;
    availableQualities: string[];
    progress: number;
    downloadProgress: number;
    conversionProgress: number | null;
    phase: DownloadPhase | null;
    speed: string;
    eta: string;
    error: string | null;
    errorCode: string | null;
    errorDetail: string | null;
    diagnosticsOpen: boolean;
    filename: string | null;
    selected: boolean;
  }

  type DownloadProgressPayload = EventMap['download-progress'];
  type DownloaderRuntimeUpdateProgressPayload = EventMap['downloader-runtime-update-progress'];
  type UpdateInstallProgressPayload = EventMap['update-install-progress'];

  function isActiveStatus(status: DownloadStatus): boolean {
    return status === 'downloading' || status === 'postprocessing' || status === 'cancelling';
  }

  function isTerminalStatus(status: DownloadStatus): boolean {
    return status === 'completed' || status === 'error' || status === 'cancelled';
  }

  function isEditablePendingStatus(status: DownloadStatus): boolean {
    return status === 'ready';
  }

  function buildQueueSummary(items: QueueItem[]) {
    const counts = {
      total: items.length,
      ready: 0,
      downloading: 0,
      completed: 0,
      failed: 0
    };

    let hasReady = false;
    let hasSelectedReady = false;
    let hasActive = false;
    let hasCompleted = false;
    let hasSelected = false;

    for (const item of items) {
      if (item.status === 'ready') {
        counts.ready += 1;
        hasReady = true;
        if (item.selected) {
          hasSelectedReady = true;
        }
      }

      if (isActiveStatus(item.status)) {
        counts.downloading += 1;
        hasActive = true;
      }

      if (item.status === 'completed') {
        counts.completed += 1;
        hasCompleted = true;
      } else if (item.status === 'cancelled') {
        hasCompleted = true;
      } else if (item.status === 'error') {
        counts.failed += 1;
      }

      if (item.selected) {
        hasSelected = true;
      }
    }

    return {
      counts,
      hasReady,
      hasSelectedReady,
      hasActive,
      hasCompleted,
      hasSelected
    };
  }

  function normalizeAppError(error: unknown): string {
    if (error && typeof error === 'object') {
      const typed = error as {
        safe_summary?: unknown;
        safeSummary?: unknown;
        summary?: unknown;
        message?: unknown;
        code?: unknown;
      };
      const summary = typed.safe_summary ?? typed.safeSummary ?? typed.summary ?? typed.message;
      if (typeof summary === 'string' && summary.trim()) {
        const code = typeof typed.code === 'string' ? ` (${typed.code})` : '';
        return `${summary}${code}`;
      }
    }
    const message = String(error ?? 'Unknown error');
    return message.startsWith('Error: ') ? message.slice(7) : message;
  }

  function appErrorDetail(error: unknown): string {
    if (error && typeof error === 'object') {
      try {
        return JSON.stringify(error);
      } catch {
        return normalizeAppError(error);
      }
    }
    return String(error ?? 'Unknown error');
  }

  function formatByteCount(bytes: number): string {
    if (!Number.isFinite(bytes) || bytes <= 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let value = bytes;
    let unitIndex = 0;

    while (value >= 1024 && unitIndex < units.length - 1) {
      value /= 1024;
      unitIndex += 1;
    }

    const digits = value >= 10 || unitIndex === 0 ? 0 : 1;
    return `${value.toFixed(digits)} ${units[unitIndex]}`;
  }

  function formatPublishedAt(value: string | null): string {
    if (!value) return 'Unknown';

    const date = new Date(value);
    if (Number.isNaN(date.getTime())) return value;

    return new Intl.DateTimeFormat(undefined, {
      year: 'numeric',
      month: 'short',
      day: 'numeric',
      hour: 'numeric',
      minute: '2-digit'
    }).format(date);
  }

  function getUpdateDownloadPercent(progress: UpdateInstallProgressPayload | null): number {
    if (!progress) return 0;
    if (progress.status === 'launching') return 100;
    if (!progress.totalBytes || progress.totalBytes <= 0) return 0;

    return Math.max(0, Math.min(100, (progress.downloadedBytes / progress.totalBytes) * 100));
  }

  function getRuntimeUpdatePercent(
    progress: DownloaderRuntimeUpdateProgressPayload | null
  ): number {
    if (!progress) return 0;
    if (progress.status === 'complete') return 100;
    if (!progress.totalBytes || progress.totalBytes <= 0) return 0;

    return Math.max(0, Math.min(100, (progress.downloadedBytes / progress.totalBytes) * 100));
  }

  function getRuntimeTool(name: string): DownloaderToolStatus | null {
    return runtimeStatus?.tools.find((tool) => tool.name === name) ?? null;
  }

  function runtimeCanDownload(): boolean {
    return runtimeAllowsDownloads(
      runtimeStatus?.state ?? null,
      backendSnapshot?.runtimeReadiness ?? null
    );
  }

  function runtimeBadgeClass(): string {
    if (!runtimeStatus || runtimeCheckState === 'checking') return 'neutral';
    if (runtimeStatus.state === 'ready') return 'ok';
    if (runtimeStatus.state === 'ready_with_warnings') return 'warn';
    return 'err';
  }

  function runtimeMissingRequiredTools(): string[] {
    return (
      runtimeStatus?.tools
        .filter((tool) => tool.required && !tool.available)
        .map((tool) => tool.name) ?? []
    );
  }

  function compactRuntimeToolVersion(tool: DownloaderToolStatus): string | null {
    const version = tool.version?.trim();
    if (!version) return null;
    const firstLine = version.split(/\r?\n/)[0]?.trim() ?? '';
    if (!firstLine) return null;

    if (tool.name === 'deno') {
      return firstLine.match(/^deno\s+([^\s]+)/i)?.[1] ?? firstLine;
    }

    if (tool.name === 'ffmpeg') {
      return firstLine.match(/^ffmpeg version\s+([^\s]+)/i)?.[1] ?? firstLine;
    }

    if (tool.name === 'ffprobe') {
      return firstLine.match(/^ffprobe version\s+([^\s]+)/i)?.[1] ?? firstLine;
    }

    return firstLine.split(/\s+/)[0] ?? firstLine;
  }

  function runtimeToolBadgeLabel(
    tool: DownloaderToolStatus | null,
    displayName: string
  ): string | null {
    if (!tool || !tool.available) return null;
    const version = compactRuntimeToolVersion(tool);
    return version ? `${displayName} ${version}` : displayName;
  }

  function runtimeBadgeText(): string {
    if (!runtimeStatus || runtimeCheckState === 'checking') return 'Runtime checking';
    const ytDlp = getRuntimeTool('yt-dlp');
    const deno = getRuntimeTool('deno');
    const missingRequired = runtimeMissingRequiredTools();
    const parts = [
      `Runtime ${runtimeStatus.state}`,
      missingRequired.length > 0 ? `Missing ${missingRequired.join(', ')}` : null,
      runtimeToolBadgeLabel(ytDlp, 'yt-dlp'),
      runtimeToolBadgeLabel(deno, 'Deno') ?? 'No Deno'
    ].filter(Boolean);
    return parts.join(' | ');
  }

  function runtimeBadgeTitle(): string {
    if (!runtimeStatus) return runtimeError ?? '';

    const toolLines = runtimeStatus.tools.map((tool) => {
      const state = tool.available
        ? (tool.version ?? 'available')
        : `missing${tool.error ? `: ${tool.error}` : ''}`;
      const source = [tool.source, tool.path].filter(Boolean).join(' ');
      return `${tool.name}: ${state} (${source})`;
    });

    return [runtimeStatus.message, ...toolLines].filter(Boolean).join('\n');
  }

  function getCompatConfigSnapshot(): string | null {
    const value = compatConfigPath.trim();
    return value ? value : null;
  }

  function getPathBasename(path: string): string {
    return path.split(/[\\/]/).pop() || path;
  }

  // -- State --
  let urlInput = $state('');
  let outputDir = $state('');
  let outputDirValidated = $state(false);
  let outputDirError = $state<string | null>(null);
  let globalQuality = $state('best');
  let globalFormat = $state<OutputFormat>('mp4');
  let queue = $state<QueueItem[]>([]);
  let appVersion = $state<string | null>(null);
  let runtimeStatus = $state<DownloaderRuntimeStatus | null>(null);
  let runtimeUpdateCheck = $state<DownloaderRuntimeUpdateCheck | null>(null);
  let runtimeCheckState = $state<'checking' | 'idle'>('checking');
  let runtimeUpdateRunning = $state(false);
  let runtimeUpdateProgress = $state<DownloaderRuntimeUpdateProgressPayload | null>(null);
  let runtimeError = $state<string | null>(null);
  let urlError = $state('');
  let useCookies = $state(false);
  let cookieMode = $state<CookieMode>('browser');
  let cookieBrowser = $state<BrowserName>('firefox');
  let cookieFilePath = $state('');
  let compatConfigPath = $state('');
  let playlistModal = $state<PlaylistModal | null>(null);
  let playlistLoading = $state(false);
  let activeInspectionId = $state<string | null>(null);
  let inspectionCancelRequested = false;
  let playlistPage = $state(0);
  let editingTitleId = $state<string | null>(null);
  let editingTitleDraft = $state('');
  let filenameEditError = $state('');
  let titleEditorInput = $state<HTMLInputElement | null>(null);
  const downloadDisplayUpdatedAt = new SvelteMap<string, number>();
  let updateCheckState = $state<'idle' | 'checking'>('idle');
  let updateInfo = $state<UpdateCheckResult | null>(null);
  let updateModalOpen = $state(false);
  let updateError = $state<string | null>(null);
  let updateInstallProgress = $state<UpdateInstallProgressPayload | null>(null);
  let updateInstallRunning = $state(false);
  let cancelAllError = $state<string | null>(null);
  let queueActionError = $state<string | null>(null);
  let startupSubsystems = $state(createStartupSubsystems());
  let startupIssues = $state<string[]>([]);
  let queueSelectAll = $state<HTMLInputElement | null>(null);
  let playlistSelectAll = $state<HTMLInputElement | null>(null);
  let queueViewport = $state<HTMLElement | null>(null);
  let queueScrollTop = $state(0);
  let queueViewportHeight = $state(600);
  let backendSnapshot = $state<AppSnapshot | null>(null);
  let backendStateError = $state<string | null>(null);
  let diagnosticsMessage = $state<string | null>(null);
  let diagnosticsError = $state<string | null>(null);
  const metadataByUrl = new SvelteMap<
    string,
    Pick<QueueItem, 'duration' | 'channel' | 'thumbnail'>
  >();
  const operationWaiters = new OperationWaitRegistry();
  const appStateController = new AppStateController(
    () => invoke('get_app_snapshot'),
    applyBackendSnapshot,
    handleBackendStateError
  );

  // -- Lifecycle --
  onMount(() => {
    let unlistenState: (() => void) | undefined;
    let unlistenProgress: (() => void) | undefined;
    let unlistenUpdateProgress: (() => void) | undefined;
    let unlistenRuntimeProgress: (() => void) | undefined;
    const queueResizeObserver =
      typeof ResizeObserver === 'undefined'
        ? undefined
        : new ResizeObserver(([entry]) => {
            if (entry) queueViewportHeight = entry.contentRect.height;
          });
    if (queueViewport) {
      queueViewportHeight = queueViewport.clientHeight || queueViewportHeight;
      queueResizeObserver?.observe(queueViewport);
    }

    const setup = async () => {
      const listenerErrors: string[] = [];

      try {
        await appStateController.start((handler) =>
          listen('app-state-changed', (event) => handler(event.payload))
        );
        unlistenState = () => appStateController.stop();
      } catch (error) {
        listenerErrors.push(`App state: ${normalizeAppError(error)}`);
      }

      try {
        unlistenProgress = await listen('download-progress', (event) => {
          const progress = event.payload;
          const idx = queue.findIndex((item) => item.downloadId === progress.download_id);
          if (idx === -1) return;

          const item = queue[idx];
          const statusChanged = item.status !== progress.status;
          const terminal = isTerminalStatus(progress.status);
          const shouldRefreshDisplay = shouldRefreshDownloadDisplay(item, progress, statusChanged);
          if (
            !shouldRefreshDisplay &&
            !statusChanged &&
            !terminal &&
            !progress.error &&
            !progress.filename
          ) {
            return;
          }
          const reduced = reduceOperationProgress(item, progress);
          const next = {
            ...reduced,
            progress: getDisplayProgress(item, progress, shouldRefreshDisplay),
            downloadProgress: terminal
              ? reduced.downloadProgress
              : getDisplayDownloadProgress(item, progress, shouldRefreshDisplay),
            conversionProgress: terminal
              ? reduced.conversionProgress
              : getDisplayConversionProgress(item, progress, shouldRefreshDisplay),
            eta: terminal ? reduced.eta : getDisplayEta(item, progress, shouldRefreshDisplay),
            error: progress.error
              ? normalizeDownloadError(progress.error, progress.error_code ?? null)
              : null,
            errorCode: progress.error_code ?? null,
            errorDetail: progress.error_detail ?? null,
            filename: progress.filename ?? item.filename
          };
          assignChangedQueueFields(queue[idx], next, [...OPERATION_PROJECTION_FIELDS, 'filename']);

          if (terminal) {
            clearProgressDisplayState(item.id);
          } else if (shouldRefreshDisplay && isActiveStatus(progress.status)) {
            downloadDisplayUpdatedAt.set(item.id, Date.now());
          }
        });
      } catch (error) {
        listenerErrors.push(`Download progress: ${normalizeAppError(error)}`);
      }

      try {
        unlistenUpdateProgress = await listen('update-install-progress', (event) => {
          updateInstallProgress = event.payload;
          if (event.payload.status === 'error') {
            updateInstallRunning = false;
            updateError = event.payload.message ?? 'Update installation failed.';
          }
        });
      } catch (error) {
        listenerErrors.push(`App update progress: ${normalizeAppError(error)}`);
      }

      try {
        unlistenRuntimeProgress = await listen('downloader-runtime-update-progress', (event) => {
          runtimeUpdateProgress = event.payload;
          if (event.payload.status === 'error') {
            runtimeUpdateRunning = false;
            runtimeError = event.payload.message ?? 'Downloader runtime update failed.';
          }
        });
      } catch (error) {
        listenerErrors.push(`Runtime update progress: ${normalizeAppError(error)}`);
      }

      if (!unlistenState || backendStateError) {
        startupIssues = [...startupIssues, ...listenerErrors];
        setStartupSubsystem('listeners', 'error');
      } else if (listenerErrors.length > 0) {
        startupIssues = [...startupIssues, ...listenerErrors];
        setStartupSubsystem('listeners', 'degraded');
      } else {
        setStartupSubsystem('listeners', 'ready');
      }

      await Promise.all([
        initializeAppVersion(),
        initializeRuntime(),
        initializeOutputDirectory(),
        initializeUpdateCheck()
      ]);
    };

    void setup();

    return () => {
      unlistenState?.();
      unlistenProgress?.();
      unlistenUpdateProgress?.();
      unlistenRuntimeProgress?.();
      queueResizeObserver?.disconnect();
      operationWaiters.rejectAll(
        new Error('Renderer was unloaded before the operation completed.')
      );
    };
  });

  // -- Helpers --
  function setStartupSubsystem(subsystem: StartupSubsystem, state: StartupSubsystemState): void {
    startupSubsystems = { ...startupSubsystems, [subsystem]: state };
  }

  function reportStartupIssue(subsystem: string, error: unknown): void {
    startupIssues = [...startupIssues, `${subsystem}: ${normalizeAppError(error)}`];
  }

  function handleBackendStateError(error: unknown): void {
    backendStateError = normalizeAppError(error);
    reportStartupIssue('App state event', error);
    setStartupSubsystem('listeners', 'error');
    operationWaiters.rejectAll(
      new Error(`The app state stream failed: ${normalizeAppError(error)}`)
    );
  }

  async function reloadAppSnapshot(): Promise<void> {
    try {
      await appStateController.reload();
    } catch (error) {
      handleBackendStateError(error);
      throw error;
    }
  }

  function applyBackendSnapshot(snapshot: AppSnapshot, delta?: StateDelta): void {
    const previousSnapshot = backendSnapshot;
    backendSnapshot = snapshot;
    backendStateError = null;
    projectBackendQueue(snapshot, previousSnapshot, delta);
    operationWaiters.settle(snapshot.operations);

    runtimeUpdateRunning = snapshot.operations.some(
      (operation) => operation.kind === 'runtime_update' && !isTerminalOperation(operation)
    );
    updateInstallRunning = snapshot.operations.some(
      (operation) => operation.kind === 'app_update' && !isTerminalOperation(operation)
    );
  }

  function queueStatus(
    record: QueueItemRecord,
    operation: OperationSnapshot | null
  ): DownloadStatus {
    if (operation) {
      if (operation.state === 'completed') return 'completed';
      if (operation.state === 'failed' || operation.state === 'interrupted') return 'error';
      if (operation.state === 'cancelled') return 'cancelled';
      if (operation.state === 'cancelling') return 'cancelling';
      if (operation.state === 'queued' || operation.state === 'starting') return 'queued';
      if (operation.state === 'running') {
        return operation.phase === 'conversion' || operation.phase === 'postprocess'
          ? 'postprocessing'
          : 'downloading';
      }
    }

    switch (record.state) {
      case 'inert':
        return 'ready';
      case 'queued':
        return 'queued';
      case 'running':
        return 'downloading';
      case 'completed':
        return 'completed';
      case 'cancelled':
        return 'cancelled';
      case 'failed':
      case 'interrupted':
        return 'error';
    }
  }

  function operationPhase(operation: OperationSnapshot | null): DownloadPhase | null {
    const phase = operation?.phase;
    return phase === 'download' ||
      phase === 'postprocess' ||
      phase === 'waiting_conversion' ||
      phase === 'conversion' ||
      phase === 'complete'
      ? phase
      : null;
  }

  function operationErrorDetail(operation: OperationSnapshot | null): string | null {
    if (!operation?.error) return null;
    return [operation.error.detail, `Correlation ID: ${operation.error.correlationId}`]
      .filter(Boolean)
      .join('\n');
  }

  function projectBackendRecord(
    snapshot: AppSnapshot,
    record: QueueItemRecord,
    existing: QueueItem | undefined
  ): QueueItem {
    const metadata = metadataByUrl.get(record.sourceUrl);
    const operation = latestOperationForItem(snapshot, record.id, record.latestOperationId);
    const status = queueStatus(record, operation);
    const terminal = isTerminalStatus(status);
    const operationProgress = Math.max(0, Math.min(100, operation?.progress ?? 0));
    const sameOperation = existing?.downloadId === operation?.id;
    const progress =
      sameOperation && isActiveStatus(status)
        ? Math.max(existing?.progress ?? 0, operationProgress)
        : status === 'completed'
          ? 100
          : operationProgress;
    const downloadProgress =
      status === 'postprocessing' || status === 'completed'
        ? 100
        : sameOperation && status === 'downloading'
          ? Math.max(existing?.downloadProgress ?? 0, operationProgress)
          : operationProgress;
    const phase = terminal ? null : operationPhase(operation);
    const conversionProgress =
      status === 'completed' && record.format === 'webm'
        ? 100
        : phase === 'conversion'
          ? operationProgress
          : terminal
            ? null
            : (existing?.conversionProgress ?? null);
    const cookie = record.cookieConfig;
    const browser = supportedBrowsers.includes(cookie?.browser as BrowserName)
      ? (cookie?.browser as BrowserName)
      : 'firefox';
    const format = [...videoFormats, ...audioFormats].includes(record.format as OutputFormat)
      ? (record.format as OutputFormat)
      : 'mp4';
    const interrupted = record.state === 'interrupted';

    return {
      id: record.id,
      downloadId: operation && !isTerminalOperation(operation) ? operation.id : null,
      url: record.sourceUrl,
      title: record.title,
      customFilename: record.filenameOverride,
      duration: existing?.duration ?? metadata?.duration ?? null,
      channel: existing?.channel ?? metadata?.channel ?? null,
      thumbnail: existing?.thumbnail ?? metadata?.thumbnail ?? null,
      infoLoaded: true,
      hasAudio: record.hasAudio,
      status,
      quality: record.quality,
      format,
      cookieConfig: cookie
        ? {
            enabled: cookie.enabled,
            mode: cookie.mode === 'file' ? 'file' : 'browser',
            browser,
            cookie_file: cookie.cookie_file
          }
        : null,
      availableQualities: [
        'best',
        ...record.availableQualities.filter((quality) => quality !== 'best')
      ],
      progress,
      downloadProgress,
      conversionProgress,
      phase,
      speed: terminal ? '' : (existing?.speed ?? ''),
      eta: terminal ? '' : (existing?.eta ?? ''),
      error:
        operation?.error?.summary ??
        (interrupted ? 'The previous app session ended before this attempt completed.' : null),
      errorCode: operation?.error?.code ?? (interrupted ? 'interrupted' : null),
      errorDetail: operationErrorDetail(operation),
      diagnosticsOpen: existing?.diagnosticsOpen ?? false,
      filename: existing?.filename ?? null,
      selected: existing?.selected ?? false
    } satisfies QueueItem;
  }

  function upsertProjectedRecord(snapshot: AppSnapshot, record: QueueItemRecord): void {
    const index = queue.findIndex((item) => item.id === record.id);
    if (index === -1) {
      queue.push(projectBackendRecord(snapshot, record, undefined));
      return;
    }
    queue[index] = projectBackendRecord(snapshot, record, queue[index]);
  }

  function assignChangedQueueFields<K extends keyof QueueItem>(
    target: QueueItem,
    source: QueueItem,
    fields: readonly K[]
  ): void {
    const writable = target as Record<keyof QueueItem, unknown>;
    const incoming = source as Record<keyof QueueItem, unknown>;
    for (const field of fields) {
      if (!Object.is(writable[field], incoming[field])) writable[field] = incoming[field];
    }
  }

  function projectOperationForRecord(snapshot: AppSnapshot, record: QueueItemRecord): void {
    const index = queue.findIndex((item) => item.id === record.id);
    if (index === -1) {
      queue.push(projectBackendRecord(snapshot, record, undefined));
      return;
    }
    const projected = projectBackendRecord(snapshot, record, queue[index]);
    assignChangedQueueFields(queue[index], projected, OPERATION_PROJECTION_FIELDS);
  }

  function projectionRelevantQueueRecordChanged(
    previous: QueueItemRecord | undefined,
    next: QueueItemRecord
  ): boolean {
    if (!previous) return true;
    return (
      previous.sourceUrl !== next.sourceUrl ||
      previous.title !== next.title ||
      previous.hasAudio !== next.hasAudio ||
      previous.format !== next.format ||
      previous.quality !== next.quality ||
      previous.outputDir !== next.outputDir ||
      previous.filenameOverride !== next.filenameOverride ||
      previous.compatConfigPath !== next.compatConfigPath ||
      previous.state !== next.state ||
      previous.latestOperationId !== next.latestOperationId ||
      previous.availableQualities.length !== next.availableQualities.length ||
      previous.availableQualities.some(
        (quality, index) => quality !== next.availableQualities[index]
      ) ||
      JSON.stringify(previous.cookieConfig) !== JSON.stringify(next.cookieConfig)
    );
  }

  function projectBackendQueue(
    snapshot: AppSnapshot,
    previousSnapshot: AppSnapshot | null,
    delta?: StateDelta
  ): void {
    if (!previousSnapshot || !delta) {
      const existingById = new Map(queue.map((item) => [item.id, item]));
      queue = snapshot.queue.map((record) =>
        projectBackendRecord(snapshot, record, existingById.get(record.id))
      );
      return;
    }

    switch (delta.kind) {
      case 'queue_item_upserted':
        if (
          !projectionRelevantQueueRecordChanged(
            previousSnapshot.queue.find((item) => item.id === delta.value.id),
            delta.value
          )
        ) {
          return;
        }
        upsertProjectedRecord(snapshot, delta.value);
        return;
      case 'queue_items_removed': {
        const removed = new Set(delta.value);
        queue = queue.filter((item) => !removed.has(item.id));
        return;
      }
      case 'operation_upserted': {
        if (!delta.value.queueItemId) return;
        const record = snapshot.queue.find((item) => item.id === delta.value.queueItemId);
        if (record) projectOperationForRecord(snapshot, record);
        return;
      }
      case 'operation_removed': {
        const previousOperation = previousSnapshot.operations.find(
          (operation) => operation.id === delta.value
        );
        if (!previousOperation?.queueItemId) return;
        const record = snapshot.queue.find((item) => item.id === previousOperation.queueItemId);
        if (record) projectOperationForRecord(snapshot, record);
        return;
      }
      case 'runtime_readiness_changed':
      case 'maintenance_changed':
        return;
    }
  }

  function waitForOperation(
    operationId: string,
    timeoutMs = 35 * 60 * 1000
  ): Promise<OperationSnapshot> {
    return operationWaiters.waitWithRefresh(
      operationId,
      () => backendSnapshot?.operations ?? [],
      timeoutMs,
      reloadAppSnapshot
    );
  }

  async function initializeAppVersion(): Promise<void> {
    try {
      appVersion = await getVersion();
      setStartupSubsystem('appVersion', 'ready');
    } catch (error) {
      appVersion = null;
      reportStartupIssue('App version', error);
      setStartupSubsystem('appVersion', 'degraded');
    }
  }

  async function initializeRuntime(): Promise<void> {
    await refreshDownloaderRuntime();
    if (!runtimeStatus) {
      reportStartupIssue('Downloader runtime', runtimeError ?? 'Unavailable');
    }
    void checkDownloaderRuntimeUpdate();
  }

  async function validateOutputDirectory(candidate: string): Promise<boolean> {
    outputDirError = null;
    if (!candidate.trim()) {
      outputDirValidated = false;
      outputDirError = 'Choose a writable output folder before downloading.';
      return false;
    }

    try {
      outputDir = await invoke('validate_output_directory', { path: candidate });
      outputDirValidated = true;
      return true;
    } catch (error) {
      outputDirValidated = false;
      outputDirError = normalizeAppError(error);
      return false;
    }
  }

  async function initializeOutputDirectory(): Promise<void> {
    try {
      const candidate = await invoke('default_download_dir');
      outputDir = candidate;
      if (await validateOutputDirectory(candidate)) {
        setStartupSubsystem('outputDirectory', 'ready');
      } else {
        reportStartupIssue('Output folder', outputDirError ?? 'Invalid folder');
        setStartupSubsystem('outputDirectory', 'error');
      }
    } catch (error) {
      outputDir = '';
      outputDirValidated = false;
      outputDirError = 'Choose a writable output folder before downloading.';
      reportStartupIssue('Output folder discovery', error);
      setStartupSubsystem('outputDirectory', 'error');
    }
  }

  async function initializeUpdateCheck(): Promise<void> {
    const succeeded = await checkForAppUpdate({
      openModal: false,
      showErrors: false
    });
    if (!succeeded) {
      startupIssues = [
        ...startupIssues,
        'App update check was unavailable; downloads remain usable.'
      ];
    }
    setStartupSubsystem('updateCheck', succeeded ? 'ready' : 'degraded');
  }

  function clearProgressDisplayState(itemId: string): void {
    downloadDisplayUpdatedAt.delete(itemId);
  }

  function clampProgressValue(value: number): number {
    return Math.min(100, Math.max(0, value));
  }

  function getDisplayProgress(
    item: QueueItem,
    payload: DownloadProgressPayload,
    shouldRefreshDisplay: boolean
  ): number {
    if (payload.status === 'completed') {
      return 100;
    }

    if (payload.status === 'postprocessing') {
      const isConversionPhase =
        payload.phase === 'conversion' || payload.conversion_progress != null;
      const conversionProgress =
        payload.conversion_progress ?? (isConversionPhase ? payload.progress : null);
      if (item.format === 'webm' && isConversionPhase && conversionProgress !== null) {
        if (!shouldRefreshDisplay) return item.progress;
        return Math.max(item.progress, clampProgressValue(conversionProgress));
      }
      return 100;
    }

    if (payload.status !== 'downloading') {
      return clampProgressValue(payload.progress);
    }

    if (!shouldRefreshDisplay) {
      return item.progress;
    }

    return Math.max(item.progress, clampProgressValue(payload.progress));
  }

  function getDisplayDownloadProgress(
    item: QueueItem,
    payload: DownloadProgressPayload,
    shouldRefreshDisplay: boolean
  ): number {
    if (payload.status === 'completed' || payload.status === 'postprocessing') {
      return 100;
    }

    if (payload.status !== 'downloading') {
      return item.downloadProgress;
    }

    const rawProgress = payload.download_progress ?? payload.progress;
    if (!shouldRefreshDisplay) {
      return item.downloadProgress;
    }

    return Math.max(item.downloadProgress, clampProgressValue(rawProgress));
  }

  function getDisplayConversionProgress(
    item: QueueItem,
    payload: DownloadProgressPayload,
    shouldRefreshDisplay: boolean
  ): number | null {
    if (payload.status === 'completed' && item.format === 'webm') {
      return 100;
    }

    if (payload.status !== 'postprocessing') {
      return item.conversionProgress;
    }

    const isConversionPhase = payload.phase === 'conversion' || payload.conversion_progress != null;

    if (!isConversionPhase || (item.format !== 'webm' && payload.conversion_progress == null)) {
      return item.conversionProgress;
    }

    const rawProgress = payload.conversion_progress ?? payload.progress;
    const currentProgress = item.conversionProgress ?? 0;
    if (!shouldRefreshDisplay) {
      return currentProgress;
    }

    return Math.max(currentProgress, clampProgressValue(rawProgress));
  }

  function shouldRefreshDownloadDisplay(
    item: QueueItem,
    payload: DownloadProgressPayload,
    statusChanged: boolean
  ): boolean {
    if (!isActiveStatus(payload.status)) {
      return true;
    }

    const now = Date.now();
    const lastUpdatedAt = downloadDisplayUpdatedAt.get(item.id) ?? 0;
    const currentProgress =
      payload.status === 'postprocessing' ? (item.conversionProgress ?? 0) : item.downloadProgress;
    return (
      statusChanged ||
      currentProgress === 0 ||
      (payload.status === 'downloading' && item.eta === '') ||
      now - lastUpdatedAt >= DOWNLOAD_DISPLAY_UPDATE_INTERVAL_MS
    );
  }

  function getDisplayEta(
    item: QueueItem,
    payload: DownloadProgressPayload,
    shouldRefreshDisplay: boolean
  ): string {
    if (payload.status !== 'downloading') {
      return '';
    }

    const nextEta = payload.eta ?? '';
    if (nextEta === '' || !shouldRefreshDisplay) {
      return item.eta;
    }

    return nextEta;
  }

  function shouldShowConversionProgress(item: QueueItem): boolean {
    return (
      item.format === 'webm' &&
      (item.status === 'postprocessing' ||
        item.conversionProgress !== null ||
        item.status === 'completed')
    );
  }

  function getStatusLabel(item: QueueItem): string {
    if (isTerminalStatus(item.status) || item.status === 'cancelling') {
      return item.status;
    }
    if (item.phase === 'waiting_conversion') return 'waiting to convert';
    if (item.status === 'postprocessing') return 'converting';
    return item.status;
  }

  function roundedProgress(value: number | null): number {
    return Math.round(clampProgressValue(value ?? 0));
  }

  function formatDuration(seconds: number | null | undefined): string {
    if (!seconds) return '--:--';

    const h = Math.floor(seconds / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    const s = Math.floor(seconds % 60);

    if (h > 0) {
      return `${h}:${String(m).padStart(2, '0')}:${String(s).padStart(2, '0')}`;
    }

    return `${m}:${String(s).padStart(2, '0')}`;
  }

  function normalizeDownloadError(message: string, code: string | null = null): string {
    if (code) return message;

    if (
      /(guest token|bad guest token|failed to query api|unauthorized)/i.test(message) &&
      /(twitter|x\.com|\[twitter\])/i.test(message)
    ) {
      return "X blocked anonymous access. Enable Cookies and make sure you're logged in, then retry.";
    }

    if (
      /saml|oauth|microsoftonline|okta|shibboleth|login required|authentication required|sign.?in to confirm|private video|members-only|age-restricted|confirm you'?re not a bot/i.test(
        message
      ) ||
      (/Unsupported URL/i.test(message) && /login|auth|sign.?in/i.test(message))
    ) {
      if (/confirm you'?re not a bot|not a bot/i.test(message)) {
        return 'YouTube requested bot verification for this public video. Update downloader runtime first, then retry.';
      }
      return "This site requires login. Enable Cookies (use Firefox or a cookies.txt file) and make sure you're logged in.";
    }

    if (
      /[Cc]ould not copy.*cookie|cookie.*database|cookies-from-browser|decrypt.*cookie|cookie.*locked/i.test(
        message
      )
    ) {
      return 'Browser cookie database is locked. Close your browser first, or switch to Firefox/cookie file mode.';
    }

    if (/cookie.*expired|cookies? are no longer valid|session expired/i.test(message)) {
      return 'Your login cookies were rejected. Refresh them from Firefox or export a new cookies.txt file and retry.';
    }

    return message;
  }

  async function refreshDownloaderRuntime(): Promise<void> {
    runtimeCheckState = 'checking';
    runtimeError = null;

    try {
      runtimeStatus = await invoke('check_downloader_runtime');
      setStartupSubsystem('runtime', runtimeStartupSubsystemState(runtimeStatus.state));
    } catch (error) {
      runtimeStatus = null;
      runtimeError = normalizeAppError(error);
      setStartupSubsystem('runtime', runtimeStartupSubsystemState(null));
    } finally {
      runtimeCheckState = 'idle';
    }
  }

  async function checkDownloaderRuntimeUpdate(): Promise<void> {
    try {
      runtimeUpdateCheck = await invoke('check_runtime_update');
    } catch {
      // Runtime availability is local and remains usable when GitHub is offline.
      runtimeUpdateCheck = null;
    }
  }

  function hasUpdateBlockingWork(): boolean {
    return queue.some((item) => isUpdateBlockingStatus(item.status));
  }

  async function updateDownloaderRuntime(): Promise<void> {
    if (updateInstallRunning) {
      runtimeError = 'Wait for the app update operation to finish.';
      return;
    }

    if (hasUpdateBlockingWork()) {
      runtimeError = 'Finish or cancel queued downloads before updating the runtime.';
      return;
    }

    runtimeUpdateRunning = true;
    runtimeError = null;
    runtimeUpdateProgress = {
      status: 'checking',
      version: runtimeUpdateCheck?.latestRuntimeVersion ?? null,
      downloadedBytes: 0,
      totalBytes: null,
      message: 'Checking downloader runtime release...'
    };

    try {
      const result = await invoke('begin_runtime_update');
      const operation = await waitForOperation(result.operationId);
      if (operation.state === 'failed') {
        throw operation.error ?? new Error('Downloader runtime update failed.');
      }
    } catch (error) {
      runtimeError = normalizeAppError(error);
    } finally {
      runtimeUpdateRunning = false;
      await refreshDownloaderRuntime();
      await checkDownloaderRuntimeUpdate();
    }
  }

  function getCookieConfig(): CookieConfig | null {
    if (!useCookies) return null;

    return {
      enabled: true,
      mode: cookieMode,
      browser: cookieBrowser,
      cookie_file: cookieMode === 'file' ? cookieFilePath || null : null
    };
  }

  function getCookieConfigSnapshot(): CookieConfig | null {
    const config = getCookieConfig();
    return config ? { ...config } : null;
  }

  function pickFirstPath(selection: string | string[] | null): string | null {
    if (typeof selection === 'string') return selection;
    if (Array.isArray(selection)) return selection[0] ?? null;
    return null;
  }

  function getQueueUrls(): Set<string> {
    return new Set(queue.map((item) => item.url));
  }

  function canRetryItem(item: QueueItem): boolean {
    return item.status === 'error' || item.status === 'cancelled';
  }

  function toggleDiagnostics(itemId: string): void {
    queue = queue.map((item) =>
      item.id === itemId ? { ...item, diagnosticsOpen: !item.diagnosticsOpen } : item
    );
  }

  function buildDiagnostics(item: QueueItem): string {
    const runtimeLines =
      runtimeStatus?.tools
        .map(
          (tool) =>
            `${tool.name}: ${tool.available ? (tool.version ?? 'available') : 'missing'} (${tool.source})`
        )
        .join('\n') ?? 'Runtime status unavailable';

    return redactDiagnosticText(
      [
        `Title: ${getQueueItemDisplayTitle(item)}`,
        `Format: ${item.format}`,
        `Quality: ${item.quality}`,
        `Status: ${item.status}`,
        `Phase: ${item.phase ?? 'n/a'}`,
        `Error code: ${item.errorCode ?? 'n/a'}`,
        `Error: ${item.error ?? 'n/a'}`,
        '',
        'Detail:',
        redactDiagnosticText(item.errorDetail ?? 'No backend detail captured.'),
        '',
        'Runtime:',
        runtimeLines,
        runtimeStatus?.message ? `Runtime message: ${runtimeStatus.message}` : ''
      ]
        .filter((line) => line !== '')
        .join('\n')
    );
  }

  async function copyDiagnostics(item: QueueItem): Promise<void> {
    await navigator.clipboard.writeText(buildDiagnostics(item));
  }

  async function enqueueItems(itemIds: string[], prioritize = false): Promise<void> {
    const uniqueIds = [...new Set(itemIds)];
    if (uniqueIds.length === 0 || !canStartDownloads) return;
    queueActionError = null;
    try {
      await invoke('enqueue_queue_items', {
        itemIds: uniqueIds,
        priority: prioritize ? 'front' : 'normal'
      });
    } catch (error) {
      queueActionError = normalizeAppError(error);
    }
  }

  function resolveQualitySelection(requestedQuality: string, availableQualities: string[]): string {
    return resolveAvailableQuality(requestedQuality, availableQualities);
  }

  function getQueueItemDisplayTitle(item: QueueItem): string {
    return item.customFilename ?? item.title;
  }

  function sanitizeFilenameDraft(value: string): string {
    let cleaned = value.trim();
    if (!cleaned) return '';

    cleaned = Array.from(cleaned, (character) =>
      character.charCodeAt(0) <= 0x1f || WINDOWS_INVALID_FILENAME_CHARS.includes(character)
        ? '_'
        : character
    ).join('');

    for (const extension of [...videoFormats, ...audioFormats]) {
      const suffix = `.${extension}`;
      if (cleaned.toLowerCase().endsWith(suffix)) {
        cleaned = cleaned.slice(0, -suffix.length);
        break;
      }
    }

    cleaned = cleaned.trim().replace(/[. ]+$/g, '');
    if (!cleaned) return '';

    const dotIndex = cleaned.indexOf('.');
    const deviceStem = (dotIndex === -1 ? cleaned : cleaned.slice(0, dotIndex)).toUpperCase();
    if (WINDOWS_RESERVED_FILENAME_STEMS.has(deviceStem)) {
      cleaned =
        dotIndex === -1
          ? `${cleaned}_`
          : `${cleaned.slice(0, dotIndex)}_${cleaned.slice(dotIndex)}`;
    }

    let bounded = '';
    let utf16Units = 0;
    for (const character of cleaned) {
      if (utf16Units + character.length > MAX_CUSTOM_FILENAME_UTF16_UNITS) break;
      bounded += character;
      utf16Units += character.length;
    }

    return bounded.trim().replace(/[. ]+$/g, '');
  }

  function canEditFilename(item: QueueItem): boolean {
    return isEditablePendingStatus(item.status);
  }

  async function beginFilenameEdit(item: QueueItem): Promise<void> {
    if (!canEditFilename(item)) return;

    if (editingTitleId && editingTitleId !== item.id) {
      await commitFilenameEdit(editingTitleId);
      if (editingTitleId) return;
    }

    editingTitleId = item.id;
    editingTitleDraft = getQueueItemDisplayTitle(item);
    filenameEditError = '';

    await tick();
    titleEditorInput?.focus();
    titleEditorInput?.select();
  }

  async function commitFilenameEdit(itemId: string | null = editingTitleId): Promise<void> {
    if (!itemId) return;

    const idx = queue.findIndex((item) => item.id === itemId);
    if (idx === -1) {
      editingTitleId = null;
      editingTitleDraft = '';
      filenameEditError = '';
      titleEditorInput = null;
      return;
    }

    const cleaned = sanitizeFilenameDraft(editingTitleDraft);
    if (!cleaned) {
      filenameEditError = 'Filename must contain at least one valid character.';
      return;
    }
    const customFilename = cleaned && cleaned !== queue[idx].title ? cleaned : null;

    try {
      await invoke('update_queue_item', {
        itemId,
        input: { filenameOverride: customFilename }
      });
      editingTitleId = null;
      editingTitleDraft = '';
      filenameEditError = '';
      titleEditorInput = null;
    } catch (error) {
      filenameEditError = normalizeAppError(error);
    }
  }

  function cancelFilenameEdit(): void {
    editingTitleId = null;
    editingTitleDraft = '';
    filenameEditError = '';
    titleEditorInput = null;
  }

  function handleFilenameEditorKeydown(event: KeyboardEvent): void {
    if (event.key === 'Enter') {
      event.preventDefault();
      void commitFilenameEdit();
    } else if (event.key === 'Escape') {
      event.preventDefault();
      cancelFilenameEdit();
    }
  }

  function closePlaylistModal(): void {
    const inspectionOperationId = playlistModal?.inspectionOperationId;
    playlistModal = null;
    playlistPage = 0;
    if (inspectionOperationId) {
      void invoke('dismiss_operation', { operationId: inspectionOperationId }).catch((error) => {
        queueActionError = `Could not dismiss completed inspection: ${normalizeAppError(error)}`;
      });
    }
  }

  function getPlaylistPageCount(): number {
    return Math.max(1, Math.ceil((playlistModal?.entries.length ?? 0) / PLAYLIST_PAGE_SIZE));
  }

  function getVisiblePlaylistEntries(): Array<{
    entry: PlaylistModalEntry;
    index: number;
  }> {
    if (!playlistModal) return [];
    const start = playlistPage * PLAYLIST_PAGE_SIZE;
    return playlistModal.entries
      .slice(start, start + PLAYLIST_PAGE_SIZE)
      .map((entry, offset) => ({ entry, index: start + offset }));
  }

  function changePlaylistPage(delta: number): void {
    playlistPage = Math.min(getPlaylistPageCount() - 1, Math.max(0, playlistPage + delta));
  }

  function openUpdateModal(): void {
    updateModalOpen = true;
  }

  function closeUpdateModal(): void {
    if (updateInstallRunning) return;
    updateModalOpen = false;
  }

  function handleQueueSelectionChange(event: Event): void {
    const checked = (event.currentTarget as HTMLInputElement).checked;
    queue = queue.map((item) => ({ ...item, selected: checked }));
  }

  function handleQueueScroll(event: Event): void {
    queueScrollTop = (event.currentTarget as HTMLElement).scrollTop;
  }

  function handlePlaylistSelectionToggle(event: Event): void {
    const checked = (event.currentTarget as HTMLInputElement).checked;
    toggleAllPlaylist(checked);
  }

  async function browseCookieFile(): Promise<void> {
    const file = pickFirstPath(
      await open({
        filters: [{ name: 'Cookie Files', extensions: ['txt'] }]
      })
    );

    if (file) cookieFilePath = file;
  }

  async function browseCompatConfigFile(): Promise<void> {
    const file = pickFirstPath(
      await open({
        multiple: false,
        filters: [
          {
            name: 'yt-dlp config',
            extensions: ['conf', 'txt']
          }
        ]
      })
    );

    if (file) compatConfigPath = file;
  }

  async function browseOutputDir(): Promise<void> {
    const dir = pickFirstPath(await open({ directory: true }));
    if (!dir) return;

    outputDir = dir;
    if (await validateOutputDirectory(dir)) {
      setStartupSubsystem('outputDirectory', 'ready');
      await Promise.all(
        queue
          .filter((item) => isEditablePendingStatus(item.status))
          .map((item) => updateQueueItemSettings(item, { outputDir }))
      );
    } else {
      setStartupSubsystem('outputDirectory', 'error');
    }
  }

  async function exportDiagnostics(): Promise<void> {
    diagnosticsError = null;
    diagnosticsMessage = null;
    const destination = await save({
      defaultPath: 'nuclear-downloader-diagnostics.jsonl',
      filters: [{ name: 'JSON Lines', extensions: ['jsonl'] }]
    });
    if (!destination) return;

    try {
      await invoke('export_diagnostics', { destination });
      diagnosticsMessage = 'Diagnostics exported successfully.';
    } catch (error) {
      diagnosticsError = normalizeAppError(error);
    }
  }

  async function clearDiagnostics(): Promise<void> {
    diagnosticsError = null;
    diagnosticsMessage = null;
    if (!window.confirm('Clear all local Nuclear Downloader diagnostics logs?')) return;
    try {
      await invoke('clear_diagnostics');
      diagnosticsMessage = 'Local diagnostics were cleared.';
    } catch (error) {
      diagnosticsError = normalizeAppError(error);
    }
  }

  async function checkForAppUpdate(options: {
    openModal: boolean;
    showErrors: boolean;
  }): Promise<boolean> {
    if (updateCheckState === 'checking') {
      if (options.openModal) updateModalOpen = true;
      return false;
    }

    updateCheckState = 'checking';
    if (options.openModal) updateModalOpen = true;
    if (options.showErrors) updateError = null;
    if (!updateInstallRunning) updateInstallProgress = null;

    try {
      const result = await invoke('check_app_update');
      updateInfo = result;
      appVersion = result.currentVersion;
      return true;
    } catch (error) {
      if (options.showErrors) {
        updateError = normalizeAppError(error);
      }
      return false;
    } finally {
      updateCheckState = 'idle';
    }
  }

  async function handleManualUpdateCheck(): Promise<void> {
    await checkForAppUpdate({ openModal: true, showErrors: true });
  }

  async function installAppUpdate(): Promise<void> {
    const targetVersion = updateInfo?.latestVersion;
    if (!targetVersion || !updateInfo?.hasUpdate || updateInstallRunning) return;

    if (runtimeUpdateRunning) {
      updateError = 'Wait for the downloader runtime update to finish.';
      updateModalOpen = true;
      return;
    }

    if (hasUpdateBlockingWork()) {
      updateError = 'Finish or cancel queued downloads before installing the app update.';
      updateModalOpen = true;
      return;
    }

    updateError = null;
    updateModalOpen = true;
    updateInstallRunning = true;
    updateInstallProgress = {
      status: 'downloading',
      version: targetVersion,
      downloadedBytes: 0,
      totalBytes: null,
      message: 'Preparing update download...'
    };

    try {
      const result = await invoke('begin_app_update', {
        expectedVersion: targetVersion
      });
      const operation = await waitForOperation(result.operationId);
      if (operation.state === 'failed') {
        throw operation.error ?? new Error('Application update failed.');
      }
    } catch (error) {
      updateError = normalizeAppError(error);
    } finally {
      updateInstallRunning = false;
    }
  }

  // -- Actions --
  async function inspectWithBackend(
    url: string,
    cookieConfig: CookieConfig | null
  ): Promise<CompletedInspection> {
    const result = await invoke('begin_inspection', {
      input: {
        url,
        cookieConfig,
        compatConfigPath: getCompatConfigSnapshot()
      }
    });
    activeInspectionId = result.operationId;
    const operation = await waitForOperation(result.operationId, 5 * 60 * 1000);
    if (operation.state === 'cancelled') throw new Error('URL inspection was cancelled.');
    if (operation.state !== 'completed' || !operation.inspectionResult) {
      throw operation.error ?? new Error('URL inspection did not produce a result.');
    }
    return {
      operationId: result.operationId,
      inspection: operation.inspectionResult as UrlInspection
    };
  }

  async function addInspectedVideo(
    info: VideoInfo,
    inspectionOperationId: string,
    cookieConfig: CookieConfig | null
  ): Promise<QueueItemRecord> {
    metadataByUrl.set(info.url, {
      duration: info.duration,
      channel: info.channel,
      thumbnail: info.thumbnail
    });
    const availableQualities = ['best', ...info.available_qualities];
    try {
      return await invoke('add_inspection_result_to_queue', {
        input: {
          inspectionOperationId,
          format: resolveAvailableFormat(globalFormat, info.has_audio, 'mp4'),
          quality: resolveQualitySelection(globalQuality, availableQualities),
          outputDir,
          cookieConfig,
          filenameOverride: null,
          compatConfigPath: getCompatConfigSnapshot()
        }
      });
    } catch (error) {
      // The authoritative inspection is single-use. If queue admission fails,
      // release its potentially large transient metadata before surfacing the
      // original error.
      await invoke('dismiss_operation', { operationId: inspectionOperationId }).catch(
        () => undefined
      );
      throw error;
    }
  }

  async function addToQueue(): Promise<void> {
    if (playlistLoading) return;

    urlError = '';
    const url = urlInput.trim();
    if (!url) return;

    if (!canStartDownloads) {
      urlError =
        startupState === 'error'
          ? 'Work is disabled because renderer event delivery could not be initialized. Restart the app.'
          : !runtimeCanDownload()
            ? 'Downloader runtime is not ready.'
            : (outputDirError ?? 'Choose a validated output folder before adding work.');
      return;
    }

    let parsedUrl: URL;
    try {
      parsedUrl = new URL(url);
    } catch {
      urlError = 'Please enter a valid URL (must start with http:// or https://)';
      return;
    }
    if (parsedUrl.protocol !== 'http:' && parsedUrl.protocol !== 'https:') {
      urlError = 'Please enter a valid URL (must start with http:// or https://)';
      return;
    }

    if (getQueueUrls().has(url)) {
      urlError = 'URL already in queue';
      return;
    }

    const cookieConfig = getCookieConfigSnapshot();
    inspectionCancelRequested = false;
    playlistLoading = true;

    try {
      const completedInspection = await inspectWithBackend(url, cookieConfig);
      const inspection = completedInspection.inspection;

      if (inspectionCancelRequested) {
        await invoke('dismiss_operation', { operationId: completedInspection.operationId });
        return;
      }

      urlInput = '';
      if (inspection.kind === 'playlist') {
        const info = inspection.playlist;
        playlistPage = 0;
        playlistModal = {
          info,
          inspectionOperationId: completedInspection.operationId,
          url,
          cookieConfig: cookieConfig ? { ...cookieConfig } : null,
          entries: info.entries.map((entry) => ({ ...entry, selected: true }))
        };
        return;
      }

      await addInspectedVideo(inspection.video, completedInspection.operationId, cookieConfig);
    } catch (error) {
      const message = normalizeDownloadError(normalizeAppError(error));
      if (!inspectionCancelRequested && !message.toLowerCase().includes('cancelled')) {
        urlError = 'Failed to inspect URL: ' + message;
      }
    } finally {
      activeInspectionId = null;
      playlistLoading = false;
      inspectionCancelRequested = false;
    }
  }

  async function cancelInspection(): Promise<void> {
    const inspectionId = activeInspectionId;
    if (!inspectionId) return;

    inspectionCancelRequested = true;
    try {
      await invoke('cancel_operation', { operationId: inspectionId });
    } catch (error) {
      urlError = 'Failed to cancel inspection: ' + normalizeDownloadError(normalizeAppError(error));
    }
  }

  async function addPlaylistSelection(): Promise<void> {
    const modal = playlistModal;
    if (!modal) return;

    const selectedEntries = modal.entries.filter((entry) => entry.selected);
    const queuedUrls = getQueueUrls();
    const cookieConfig = modal.cookieConfig ? { ...modal.cookieConfig } : null;
    urlInput = '';
    closePlaylistModal();
    playlistLoading = true;
    inspectionCancelRequested = false;
    const failures: string[] = [];

    try {
      for (const entry of selectedEntries) {
        if (inspectionCancelRequested) break;
        if (queuedUrls.has(entry.url)) continue;
        try {
          const completedInspection = await inspectWithBackend(entry.url, cookieConfig);
          const inspection = completedInspection.inspection;
          if (inspection.kind !== 'video') {
            await invoke('dismiss_operation', { operationId: completedInspection.operationId });
            throw new Error('A selected playlist entry unexpectedly resolved to another playlist.');
          }
          await addInspectedVideo(inspection.video, completedInspection.operationId, cookieConfig);
          queuedUrls.add(entry.url);
        } catch (error) {
          if (inspectionCancelRequested) break;
          failures.push(`${entry.title ?? entry.id}: ${normalizeAppError(error)}`);
        }
      }
    } finally {
      activeInspectionId = null;
      playlistLoading = false;
      inspectionCancelRequested = false;
    }

    if (failures.length > 0) {
      urlError = `Some playlist entries could not be added. ${failures.slice(0, 3).join(' ')}`;
    }
  }

  function toggleAllPlaylist(checked: boolean): void {
    const modal = playlistModal;
    if (!modal) return;

    playlistModal = {
      ...modal,
      entries: modal.entries.map((entry) => ({
        ...entry,
        selected: checked
      }))
    };
  }

  async function downloadItem(item: QueueItem): Promise<void> {
    if (!canStartDownloads) return;
    if (!isEditablePendingStatus(item.status)) return;
    await enqueueItems([item.id], true);
  }

  async function downloadAll(): Promise<void> {
    if (!canStartDownloads) return;
    const readyIds = queue.filter((item) => item.status === 'ready').map((item) => item.id);
    await enqueueItems(readyIds);
  }

  async function downloadSelected(): Promise<void> {
    if (!canStartDownloads) return;
    const selectedIds = queue
      .filter((item) => item.selected && item.status === 'ready')
      .map((item) => item.id);
    await enqueueItems(selectedIds);
  }

  async function cancelItem(item: QueueItem): Promise<void> {
    if (!item.downloadId) return;

    const idx = queue.findIndex((queueItem) => queueItem.id === item.id);
    if (idx === -1) return;
    const previousStatus = queue[idx].status;
    queue[idx] = {
      ...queue[idx],
      status: 'cancelling',
      error: null,
      errorCode: null,
      errorDetail: null
    };

    try {
      await invoke('cancel_operation', { operationId: item.downloadId });
    } catch (error) {
      const currentIdx = queue.findIndex((queueItem) => queueItem.id === item.id);
      if (currentIdx !== -1 && queue[currentIdx].status === 'cancelling') {
        queue[currentIdx] = {
          ...queue[currentIdx],
          status: previousStatus,
          error: `Cancellation failed: ${normalizeAppError(error)}`,
          errorCode: 'cancel_failed',
          errorDetail: appErrorDetail(error),
          diagnosticsOpen: true
        };
      }
    }
  }

  async function cancelAll(): Promise<void> {
    cancelAllError = null;

    try {
      const result = await invoke('cancel_all_downloads');
      if (!result.idle) {
        const count = result.remainingOperationIds.length;
        cancelAllError = `Cancellation timed out with ${count} operation${count === 1 ? '' : 's'} still stopping. New work remains paused.`;
      }
    } catch (error) {
      cancelAllError = `Cancel all did not drain cleanly: ${normalizeAppError(error)}`;
    }
  }

  async function retryItem(item: QueueItem): Promise<void> {
    if (!canRetryItem(item)) return;
    clearProgressDisplayState(item.id);
    await enqueueItems([item.id], true);
  }

  async function removeSelected(): Promise<void> {
    const removableIds = queue
      .filter((item) => item.selected && !isActiveStatus(item.status))
      .map((item) => item.id);
    if (removableIds.length === 0) return;
    queueActionError = null;
    try {
      await invoke('remove_queue_items', { itemIds: removableIds });
      if (editingTitleId && removableIds.includes(editingTitleId)) cancelFilenameEdit();
    } catch (error) {
      queueActionError = normalizeAppError(error);
    }
  }

  async function clearCompleted(): Promise<void> {
    const itemIds = queue
      .filter((item) => item.status === 'completed' || item.status === 'cancelled')
      .map((item) => item.id);
    if (itemIds.length === 0) return;
    try {
      await invoke('remove_queue_items', { itemIds });
    } catch (error) {
      queueActionError = normalizeAppError(error);
    }
  }

  async function updateQueueItemSettings(
    item: QueueItem,
    input: {
      format?: OutputFormat;
      quality?: string;
      outputDir?: string;
      filenameOverride?: string | null;
    }
  ): Promise<void> {
    queueActionError = null;
    try {
      await invoke('update_queue_item', { itemId: item.id, input });
    } catch (error) {
      queueActionError = normalizeAppError(error);
      await reloadAppSnapshot().catch(() => undefined);
    }
  }

  async function applyGlobalQuality(): Promise<void> {
    await Promise.all(
      queue
        .filter((item) => isEditablePendingStatus(item.status))
        .map((item) =>
          updateQueueItemSettings(item, {
            quality: resolveQualitySelection(globalQuality, item.availableQualities)
          })
        )
    );
  }

  async function applyGlobalFormat(): Promise<void> {
    await Promise.all(
      queue
        .filter((item) => isEditablePendingStatus(item.status))
        .map((item) =>
          updateQueueItemSettings(item, {
            format: resolveAvailableFormat(globalFormat, item.hasAudio, 'mp4')
          })
        )
    );
  }

  async function handleItemQualityChange(item: QueueItem, event: Event): Promise<void> {
    const quality = (event.currentTarget as HTMLSelectElement).value;
    await updateQueueItemSettings(item, { quality });
  }

  async function handleItemFormatChange(item: QueueItem, event: Event): Promise<void> {
    const requested = (event.currentTarget as HTMLSelectElement).value as OutputFormat;
    await updateQueueItemSettings(item, {
      format: resolveAvailableFormat(requested, item.hasAudio, 'mp4')
    });
  }

  function handleUrlSubmit(event: SubmitEvent): void {
    event.preventDefault();
    void addToQueue();
  }

  // -- Derived --
  let queueSummary = $derived(buildQueueSummary(queue));
  let startupState = $derived(deriveStartupState(startupSubsystems));
  let maintenanceActive = $derived(
    backendSnapshot?.maintenanceActive ?? (runtimeUpdateRunning || updateInstallRunning)
  );
  let canStartDownloads = $derived(
    canStartWork({
      runtimeReady:
        runtimeCanDownload() &&
        startupState !== 'error' &&
        backendSnapshot !== null &&
        !backendSnapshot.draining,
      outputDirectoryReady: outputDirValidated,
      maintenanceActive
    })
  );
  let queueSelectionState = $derived(deriveSelectionState(queue));
  let queueWindowStart = $derived(
    Math.max(0, Math.floor(queueScrollTop / QUEUE_ROW_HEIGHT_PX) - QUEUE_ROW_OVERSCAN)
  );
  let queueWindowEnd = $derived(
    Math.min(
      queue.length,
      Math.ceil((queueScrollTop + queueViewportHeight) / QUEUE_ROW_HEIGHT_PX) + QUEUE_ROW_OVERSCAN
    )
  );
  let visibleQueueRows = $derived(
    queue.slice(queueWindowStart, queueWindowEnd).map((item, offset) => ({
      item,
      index: queueWindowStart + offset
    }))
  );
  let queueTopSpacerHeight = $derived(queueWindowStart * QUEUE_ROW_HEIGHT_PX);
  let queueBottomSpacerHeight = $derived(
    Math.max(0, (queue.length - queueWindowEnd) * QUEUE_ROW_HEIGHT_PX)
  );
  let playlistSelectionState = $derived(deriveSelectionState(playlistModal?.entries ?? []));

  $effect(() => {
    const maxScrollTop = Math.max(0, queue.length * QUEUE_ROW_HEIGHT_PX - queueViewportHeight);
    if (queueScrollTop > maxScrollTop) {
      queueScrollTop = maxScrollTop;
      if (queueViewport) queueViewport.scrollTop = maxScrollTop;
    }
  });

  $effect(() => {
    if (queueSelectAll) {
      queueSelectAll.indeterminate = queueSelectionState === 'some';
    }
    if (playlistSelectAll) {
      playlistSelectAll.indeterminate = playlistSelectionState === 'some';
    }
  });
</script>

<main>
  <!-- Header -->
  <header>
    <h1>Nuclear Downloader</h1>
    <div class="header-tools">
      <div class="status-badges">
        {#if appVersion}
          <span class="badge neutral">v{appVersion}</span>
        {/if}
        <span
          class="badge {runtimeBadgeClass()}"
          title={runtimeBadgeTitle()}
          data-testid="runtime-status"
        >
          {runtimeBadgeText()}
        </span>
        {#if maintenanceActive}
          <span class="badge warn" role="status" aria-live="polite">
            {backendSnapshot?.draining ? 'Cancelling work…' : 'Maintenance active'}
          </span>
        {/if}
        {#if runtimeUpdateCheck?.updateAvailable}
          <button
            type="button"
            class="badge-button"
            onclick={updateDownloaderRuntime}
            disabled={maintenanceActive || hasUpdateBlockingWork()}
            title={hasUpdateBlockingWork()
              ? 'Finish or cancel queued downloads first'
              : (runtimeUpdateCheck.message ?? '')}
          >
            {runtimeUpdateRunning ? 'Runtime...' : 'Update Runtime'}
          </button>
        {/if}
        {#if updateInfo?.hasUpdate && updateInfo.latestVersion}
          <button
            type="button"
            class="badge-button"
            onclick={openUpdateModal}
            disabled={updateCheckState === 'checking' || maintenanceActive}
          >
            Update v{updateInfo.latestVersion}
          </button>
        {/if}
      </div>
      <button
        class="small header-action"
        onclick={refreshDownloaderRuntime}
        disabled={runtimeCheckState === 'checking' || maintenanceActive}
      >
        {runtimeCheckState === 'checking' ? 'Runtime...' : 'Check Runtime'}
      </button>
      <button
        class="small header-action"
        onclick={handleManualUpdateCheck}
        disabled={updateCheckState === 'checking' || maintenanceActive}
      >
        {#if updateInstallRunning}
          Installing...
        {:else if updateCheckState === 'checking'}
          Checking...
        {:else}
          Check for Updates
        {/if}
      </button>
    </div>
  </header>

  {#if startupState === 'error' || startupState === 'degraded'}
    <section
      class="startup-status {startupState}"
      role={startupState === 'error' ? 'alert' : 'status'}
      aria-live={startupState === 'error' ? 'assertive' : 'polite'}
    >
      <strong>
        {startupState === 'error'
          ? 'Startup requires attention.'
          : 'Started with limited functionality.'}
      </strong>
      {#if startupIssues.length > 0}
        <span>{startupIssues.join(' ')}</span>
      {/if}
    </section>
  {/if}

  <!-- URL Input -->
  <form class="url-bar" autocomplete="off" onsubmit={handleUrlSubmit}>
    <label class="sr-only" for="video-url">Video or playlist URL</label>
    <input
      id="video-url"
      type="text"
      name="nuclear-source-url"
      placeholder="Paste a video URL..."
      bind:value={urlInput}
      autocomplete="off"
      autocapitalize="none"
      spellcheck={false}
      inputmode="url"
      aria-autocomplete="none"
      disabled={playlistLoading || maintenanceActive}
      class:input-error={Boolean(urlError)}
      aria-describedby={urlError ? 'url-error' : undefined}
    />
    <button type="submit" class="primary" disabled={!canStartDownloads || playlistLoading}>
      {playlistLoading ? 'Loading...' : 'Add'}
    </button>
    {#if playlistLoading}
      <button onclick={cancelInspection}>Cancel</button>
    {/if}
    {#if urlError}
      <span id="url-error" class="error-text" role="alert" aria-live="assertive">{urlError}</span>
    {/if}
    {#if runtimeError}
      <span class="error-text" role="alert" aria-live="assertive">{runtimeError}</span>
    {:else if runtimeStatus?.message && runtimeStatus.state !== 'ready'}
      <span class="error-text">{runtimeStatus.message}</span>
    {/if}
    {#if runtimeUpdateProgress}
      <span class="muted" role="status" aria-live="polite">
        {runtimeUpdateProgress.message ?? 'Runtime update'}
        {Math.round(getRuntimeUpdatePercent(runtimeUpdateProgress))}%
      </span>
    {/if}
  </form>

  <!-- Settings Row -->
  <section class="settings-row">
    <div class="setting">
      <label for="quality">Quality</label>
      <select id="quality" bind:value={globalQuality} onchange={applyGlobalQuality}>
        <option value="best">Best</option>
        <option value="2160p">4K</option>
        <option value="1440p">1440p</option>
        <option value="1080p">1080p</option>
        <option value="720p">720p</option>
        <option value="480p">480p</option>
        <option value="360p">360p</option>
      </select>
    </div>
    <div class="setting">
      <label for="format">Format</label>
      <select id="format" bind:value={globalFormat} onchange={applyGlobalFormat}>
        <optgroup label="Video">
          {#each videoFormats as fmt (fmt)}
            <option value={fmt}>{fmt.toUpperCase()}</option>
          {/each}
        </optgroup>
        <optgroup label="Audio Only">
          {#each audioFormats as fmt (fmt)}
            <option value={fmt}>{fmt.toUpperCase()}</option>
          {/each}
        </optgroup>
      </select>
    </div>
    <div class="setting output-dir">
      <label for="outdir">Output</label>
      <input id="outdir" type="text" bind:value={outputDir} readonly />
      <button onclick={browseOutputDir}>Browse</button>
      {#if outputDirError}
        <span class="error-text" role="alert">{outputDirError}</span>
      {/if}
    </div>
    <div class="setting cookie-setting">
      <label>
        <input type="checkbox" bind:checked={useCookies} />
        Cookies
      </label>
      {#if useCookies}
        <label class="sr-only" for="cookie-mode">Cookie source</label>
        <select id="cookie-mode" bind:value={cookieMode} class="cookie-mode-select">
          <option value="browser">From Browser</option>
          <option value="file">From File</option>
        </select>
        {#if cookieMode === 'browser'}
          <label class="sr-only" for="cookie-browser">Cookie browser</label>
          <select id="cookie-browser" bind:value={cookieBrowser}>
            {#each supportedBrowsers as b (b)}
              <option value={b}>{b.charAt(0).toUpperCase() + b.slice(1)}</option>
            {/each}
          </select>
          {#if cookieBrowser === 'chrome' || cookieBrowser === 'edge' || cookieBrowser === 'brave' || cookieBrowser === 'chromium'}
            <span class="cookie-warn"
              >Chromium browsers block cookie access — use Firefox or a cookie file instead</span
            >
          {:else}
            <span class="cookie-hint">Close {cookieBrowser} first if errors occur</span>
          {/if}
        {:else}
          <button class="cookie-browse" onclick={browseCookieFile}>
            {cookieFilePath ? cookieFilePath.split(/[\\/]/).pop() : 'Select cookies.txt'}
          </button>
          <span class="cookie-hint"
            >Export via browser extension (e.g. "Get cookies.txt LOCALLY")</span
          >
        {/if}
      {/if}
    </div>
    <div class="setting advanced-config">
      <label for="compat-config">Compat Config</label>
      <button id="compat-config" class="cookie-browse" onclick={browseCompatConfigFile}>
        {compatConfigPath ? getPathBasename(compatConfigPath) : 'None'}
      </button>
      {#if compatConfigPath}
        <button class="small" onclick={() => (compatConfigPath = '')}>Clear</button>
      {/if}
    </div>
  </section>

  <!-- Action Buttons -->
  <section class="actions">
    <button
      class="primary"
      onclick={downloadAll}
      disabled={!canStartDownloads || !queueSummary.hasReady}>Download All</button
    >
    <button
      onclick={downloadSelected}
      disabled={!canStartDownloads || !queueSummary.hasSelectedReady}>Download Selected</button
    >
    <button onclick={removeSelected} disabled={!queueSummary.hasSelected}>Remove Selected</button>
    <button onclick={clearCompleted} disabled={!queueSummary.hasCompleted}>Clear Done</button>
    <button class="danger" onclick={cancelAll} disabled={!queueSummary.hasActive}>Cancel All</button
    >
    <button onclick={exportDiagnostics}>Export Diagnostics</button>
    <button onclick={clearDiagnostics}>Clear Diagnostics</button>
    {#if cancelAllError}
      <span class="error-text" role="alert" aria-live="assertive">{cancelAllError}</span>
    {/if}
    {#if queueActionError}
      <span class="error-text" role="alert" aria-live="assertive">{queueActionError}</span>
    {/if}
    {#if diagnosticsError}
      <span class="error-text" role="alert" aria-live="assertive">{diagnosticsError}</span>
    {:else if diagnosticsMessage}
      <span class="muted" role="status" aria-live="polite">{diagnosticsMessage}</span>
    {/if}
  </section>

  <!-- Queue Table -->
  <section
    class="queue"
    bind:this={queueViewport}
    onscroll={handleQueueScroll}
    data-queue-count={queue.length}
  >
    {#if queue.length === 0}
      <div class="empty-state">
        <p>No videos in queue. Paste a video URL above to get started.</p>
      </div>
    {:else}
      <table aria-rowcount={queue.length + 1}>
        <thead>
          <tr>
            <th class="col-check">
              <input
                bind:this={queueSelectAll}
                type="checkbox"
                checked={queueSelectionState === 'all'}
                onchange={handleQueueSelectionChange}
                aria-label="Select all queue items"
              />
            </th>
            <th class="col-title">Title</th>
            <th class="col-status">Status</th>
            <th class="col-quality">Quality</th>
            <th class="col-format">Format</th>
            <th class="col-progress">Progress</th>
            <th class="col-speed">Speed</th>
            <th class="col-eta">ETA</th>
            <th class="col-actions"></th>
          </tr>
        </thead>
        <tbody>
          {#if queueTopSpacerHeight > 0}
            <tr class="virtual-spacer" aria-hidden="true">
              <td colspan="9" style={`height: ${queueTopSpacerHeight}px`}></td>
            </tr>
          {/if}
          {#each visibleQueueRows as row (row.item.id)}
            {@const item = row.item}
            {@const i = row.index}
            <tr
              class="queue-item"
              class:downloading={item.status === 'downloading'}
              aria-rowindex={i + 2}
            >
              <td class="col-check">
                <input
                  type="checkbox"
                  bind:checked={queue[i].selected}
                  aria-label={`Select ${getQueueItemDisplayTitle(item)}`}
                />
              </td>
              <td class="col-title" title={item.url}>
                <div class="title-cell">
                  {#if item.thumbnail}
                    <img
                      src={item.thumbnail}
                      alt=""
                      class="thumb"
                      loading="lazy"
                      decoding="async"
                      referrerpolicy="no-referrer"
                    />
                  {/if}
                  <div class="title-info">
                    {#if editingTitleId === item.id}
                      <input
                        bind:this={titleEditorInput}
                        bind:value={editingTitleDraft}
                        type="text"
                        class="title-editor"
                        aria-label="Edit queued filename"
                        onblur={() => commitFilenameEdit(item.id)}
                        onkeydown={handleFilenameEditorKeydown}
                        onclick={(event) => event.stopPropagation()}
                      />
                      {#if filenameEditError}
                        <span class="filename-error" role="alert">{filenameEditError}</span>
                      {/if}
                    {:else if canEditFilename(item)}
                      <button
                        type="button"
                        class="title-button"
                        title="Click to edit the filename before download"
                        onclick={() => beginFilenameEdit(item)}
                      >
                        <span class="title-text">{getQueueItemDisplayTitle(item)}</span>
                      </button>
                    {:else}
                      <span class="title-text">{getQueueItemDisplayTitle(item)}</span>
                    {/if}
                    {#if item.channel}
                      <span class="channel">{item.channel}</span>
                    {/if}
                    {#if item.duration}
                      <span class="duration">{formatDuration(item.duration)}</span>
                    {/if}
                  </div>
                </div>
              </td>
              <td class="col-status">
                <span class="status-pill {item.status}">
                  {getStatusLabel(item)}
                </span>
                {#if item.error}
                  <button
                    type="button"
                    class="error-tooltip"
                    title="Show diagnostics"
                    onclick={() => toggleDiagnostics(item.id)}
                  >
                    !
                  </button>
                  <span class="error-summary" title={item.error} role="alert">
                    {item.error}
                  </span>
                {/if}
              </td>
              <td class="col-quality">
                {#if isEditablePendingStatus(item.status)}
                  <select
                    value={item.quality}
                    onchange={(event) => handleItemQualityChange(item, event)}
                    aria-label={`Quality for ${getQueueItemDisplayTitle(item)}`}
                  >
                    {#each item.availableQualities as q (q)}
                      <option value={q}>{q === 'best' ? 'Best' : q}</option>
                    {/each}
                  </select>
                {:else}
                  <span class="muted">{item.quality}</span>
                {/if}
              </td>
              <td class="col-format">
                {#if isEditablePendingStatus(item.status)}
                  <select
                    value={item.format}
                    onchange={(event) => handleItemFormatChange(item, event)}
                    aria-label={`Format for ${getQueueItemDisplayTitle(item)}`}
                  >
                    <optgroup label="Video">
                      {#each videoFormats as fmt (fmt)}
                        <option value={fmt}>{fmt.toUpperCase()}</option>
                      {/each}
                    </optgroup>
                    <optgroup label="Audio">
                      {#each audioFormats as fmt (fmt)}
                        <option
                          value={fmt}
                          disabled={item.hasAudio === false && isAudioOnlyFormat(fmt)}
                        >
                          {fmt.toUpperCase()}
                        </option>
                      {/each}
                    </optgroup>
                  </select>
                {:else}
                  <span class="muted">{item.format.toUpperCase()}</span>
                {/if}
              </td>
              <td class="col-progress">
                {#if shouldShowConversionProgress(item)}
                  <div class="phase-progress">
                    <div class="phase-progress-row">
                      <span class="phase-label">DL</span>
                      <div class="progress-bar">
                        <div
                          class="progress-fill"
                          class:complete={item.downloadProgress >= 100}
                          class:error={item.status === 'error' && item.conversionProgress === null}
                          style="width: {item.downloadProgress}%"
                        ></div>
                        <span class="progress-text">{roundedProgress(item.downloadProgress)}%</span>
                      </div>
                    </div>
                    <div class="phase-progress-row">
                      <span class="phase-label">CV</span>
                      <div class="progress-bar">
                        <div
                          class="progress-fill convert"
                          class:complete={item.status === 'completed'}
                          class:error={item.status === 'error' && item.conversionProgress !== null}
                          style="width: {item.conversionProgress ?? 0}%"
                        ></div>
                        <span class="progress-text"
                          >{roundedProgress(item.conversionProgress)}%</span
                        >
                      </div>
                    </div>
                  </div>
                {:else}
                  <div class="progress-bar">
                    <div
                      class="progress-fill"
                      class:complete={item.status === 'completed'}
                      class:error={item.status === 'error'}
                      style="width: {item.progress}%"
                    ></div>
                    <span class="progress-text">{roundedProgress(item.progress)}%</span>
                  </div>
                {/if}
              </td>
              <td class="col-speed">
                <span class="muted">{item.speed}</span>
              </td>
              <td class="col-eta">
                <span class="muted">{item.eta}</span>
              </td>
              <td class="col-actions">
                {#if isEditablePendingStatus(item.status)}
                  <button
                    class="small primary"
                    onclick={() => downloadItem(item)}
                    disabled={!canStartDownloads}
                    aria-label={`Download ${getQueueItemDisplayTitle(item)}`}>DL</button
                  >
                {:else if item.status === 'downloading' || item.status === 'postprocessing' || item.status === 'cancelling'}
                  <button
                    class="small danger"
                    onclick={() => cancelItem(item)}
                    disabled={item.status === 'cancelling'}
                    aria-label={`Cancel ${getQueueItemDisplayTitle(item)}`}
                    >{item.status === 'cancelling' ? '...' : 'X'}</button
                  >
                {:else if canRetryItem(item)}
                  <button class="small" onclick={() => retryItem(item)}>Retry</button>
                {/if}
              </td>
            </tr>
            {#if item.diagnosticsOpen && item.error}
              <tr class="diagnostics-row">
                <td colspan="9">
                  <div class="diagnostics-panel">
                    <div class="diagnostics-header">
                      <span>{item.errorCode ?? 'download_failed'}</span>
                      <button class="small" onclick={() => copyDiagnostics(item)}>
                        Copy Diagnostics
                      </button>
                    </div>
                    <pre>{redactDiagnosticText(item.errorDetail ?? item.error)}</pre>
                  </div>
                </td>
              </tr>
            {/if}
          {/each}
          {#if queueBottomSpacerHeight > 0}
            <tr class="virtual-spacer" aria-hidden="true">
              <td colspan="9" style={`height: ${queueBottomSpacerHeight}px`}></td>
            </tr>
          {/if}
        </tbody>
      </table>
    {/if}
  </section>

  <!-- Status Bar -->
  <footer role="status" aria-live="polite">
    <span>{queueSummary.counts.total} items</span>
    <span class="sep">|</span>
    <span>{queueSummary.counts.ready} ready</span>
    <span class="sep">|</span>
    <span>{queueSummary.counts.downloading} downloading</span>
    <span class="sep">|</span>
    <span>{queueSummary.counts.completed} done</span>
    {#if queueSummary.counts.failed > 0}
      <span class="sep">|</span>
      <span class="error-text">{queueSummary.counts.failed} failed</span>
    {/if}
  </footer>
</main>

<!-- Playlist Picker Modal -->
{#if playlistModal}
  <div class="modal-layer">
    <button
      type="button"
      class="modal-backdrop"
      aria-label="Close playlist picker"
      onclick={closePlaylistModal}
    ></button>
    <div
      class="modal"
      role="dialog"
      aria-modal="true"
      aria-labelledby="playlist-modal-title"
      tabindex="-1"
      use:accessibleDialog={{ onClose: closePlaylistModal }}
    >
      <div class="modal-header">
        <div>
          <h2 id="playlist-modal-title">{playlistModal.info.title}</h2>
          {#if playlistModal.info.channel}
            <span class="modal-channel">{playlistModal.info.channel}</span>
          {/if}
          <span class="modal-count">
            {playlistModal.info.truncated
              ? `Showing first ${playlistModal.info.entry_count} videos`
              : `${playlistModal.info.entry_count} videos`}
          </span>
        </div>
        <button class="small" onclick={closePlaylistModal} data-dialog-initial-focus
          >Close playlist picker</button
        >
      </div>
      <div class="modal-controls">
        <label class="select-all-label">
          <input
            bind:this={playlistSelectAll}
            type="checkbox"
            checked={playlistSelectionState === 'all'}
            onchange={handlePlaylistSelectionToggle}
          />
          Select All
        </label>
        <span class="muted">
          {playlistModal.entries.filter((entry) => entry.selected).length} of {playlistModal.entries
            .length} selected
        </span>
      </div>
      <div class="modal-list">
        {#each getVisiblePlaylistEntries() as row (row.entry.url)}
          {@const entry = row.entry}
          <label class="playlist-entry" class:entry-selected={entry.selected}>
            <input type="checkbox" bind:checked={playlistModal.entries[row.index].selected} />
            {#if entry.thumbnail}
              <img
                src={entry.thumbnail}
                alt=""
                class="entry-thumb"
                loading="lazy"
                decoding="async"
                referrerpolicy="no-referrer"
              />
            {/if}
            <div class="entry-info">
              <span class="entry-title">{entry.title || entry.id}</span>
              {#if entry.duration}
                <span class="entry-duration">{formatDuration(entry.duration)}</span>
              {/if}
            </div>
          </label>
        {/each}
      </div>
      {#if getPlaylistPageCount() > 1}
        <div class="modal-pagination">
          <button class="small" onclick={() => changePlaylistPage(-1)} disabled={playlistPage === 0}
            >Previous</button
          >
          <span class="muted">Page {playlistPage + 1} of {getPlaylistPageCount()}</span>
          <button
            class="small"
            onclick={() => changePlaylistPage(1)}
            disabled={playlistPage >= getPlaylistPageCount() - 1}>Next</button
          >
        </div>
      {/if}
      <div class="modal-footer">
        <button
          class="primary"
          onclick={addPlaylistSelection}
          disabled={!playlistModal.entries.some((entry) => entry.selected)}
        >
          Add {playlistModal.entries.filter((entry) => entry.selected).length} Videos to Queue
        </button>
        <button onclick={closePlaylistModal}>Cancel</button>
      </div>
    </div>
  </div>
{/if}

<!-- Update Modal -->
{#if updateModalOpen}
  <div class="modal-layer">
    <button
      type="button"
      class="modal-backdrop"
      aria-label="Close update dialog"
      onclick={closeUpdateModal}
      disabled={updateInstallRunning}
    ></button>
    <div
      class="modal update-modal"
      role="dialog"
      aria-modal="true"
      aria-labelledby="update-modal-title"
      tabindex="-1"
      use:accessibleDialog={{ onClose: closeUpdateModal, locked: updateInstallRunning }}
    >
      <div class="modal-header">
        <div>
          <h2 id="update-modal-title">App Updates</h2>
          <span class="modal-count">GitHub Releases installer update</span>
        </div>
        <button
          class="small"
          onclick={closeUpdateModal}
          disabled={updateInstallRunning}
          data-dialog-initial-focus
        >
          Close
        </button>
      </div>
      <div class="update-body">
        {#if updateCheckState === 'checking' && !updateInfo}
          <p class="update-summary">Checking the latest stable GitHub Release...</p>
        {:else}
          <div class="update-meta">
            <div class="update-meta-row">
              <span class="update-meta-label">Current</span>
              <span class="update-meta-value"
                >v{appVersion ?? updateInfo?.currentVersion ?? 'Unknown'}</span
              >
            </div>
            <div class="update-meta-row">
              <span class="update-meta-label">Latest</span>
              <span class="update-meta-value">
                {#if updateInfo?.latestVersion}
                  v{updateInfo.latestVersion}
                {:else}
                  Unknown
                {/if}
              </span>
            </div>
            <div class="update-meta-row">
              <span class="update-meta-label">Published</span>
              <span class="update-meta-value">
                {formatPublishedAt(updateInfo?.publishedAt ?? null)}
              </span>
            </div>
            <div class="update-meta-row">
              <span class="update-meta-label">Installer</span>
              <span class="update-meta-value">
                {updateInfo?.installerName ?? 'Checked during install'}
              </span>
            </div>
          </div>

          {#if updateInfo?.hasUpdate}
            <p class="update-summary">
              A newer version is available. Installing it downloads the published Windows NSIS
              installer, closes the app, and relaunches Nuclear Downloader automatically.
            </p>
          {:else if updateInfo}
            <p class="update-summary">You are already on the latest stable release.</p>
          {/if}

          {#if updateInstallProgress}
            <div class="update-progress-panel">
              <div class="update-progress-header">
                <span>{updateInstallProgress.message ?? 'Working...'}</span>
                <span>
                  {#if updateInstallProgress.totalBytes}
                    {formatByteCount(updateInstallProgress.downloadedBytes)} / {formatByteCount(
                      updateInstallProgress.totalBytes
                    )}
                  {:else if updateInstallProgress.downloadedBytes > 0}
                    {formatByteCount(updateInstallProgress.downloadedBytes)}
                  {:else}
                    Waiting...
                  {/if}
                </span>
              </div>
              <div class="update-progress-bar">
                <div
                  class="update-progress-fill"
                  style="width: {getUpdateDownloadPercent(updateInstallProgress)}%"
                ></div>
              </div>
            </div>
          {/if}

          {#if updateError}
            <p class="update-error" role="alert" aria-live="assertive">{updateError}</p>
          {/if}

          <div class="update-notes-block">
            <h3>Release Notes</h3>
            <div class="update-notes">
              {updateInfo?.notes ?? 'No release notes were provided for this release.'}
            </div>
          </div>
        {/if}
      </div>
      <div class="modal-footer">
        {#if updateInfo?.hasUpdate && updateInfo.latestVersion}
          <button
            class="primary"
            onclick={installAppUpdate}
            disabled={maintenanceActive ||
              updateCheckState === 'checking' ||
              hasUpdateBlockingWork()}
            title={hasUpdateBlockingWork() ? 'Finish or cancel queued downloads first' : ''}
          >
            {#if updateInstallRunning}
              Installing...
            {:else}
              Install v{updateInfo.latestVersion}
            {/if}
          </button>
        {/if}
        <button
          onclick={handleManualUpdateCheck}
          disabled={updateCheckState === 'checking' || maintenanceActive}
        >
          {updateCheckState === 'checking' ? 'Checking...' : 'Refresh Check'}
        </button>
      </div>
    </div>
  </div>
{/if}

<style>
  /* -- Catppuccin Mocha Palette -- */
  :root {
    --crust: #11111b;
    --mantle: #181825;
    --base: #1e1e2e;
    --surface0: #313244;
    --surface1: #45475a;
    --surface2: #585b70;
    --overlay0: #6c7086;
    --text: #cdd6f4;
    --subtext0: #a6adc8;
    --subtext1: #bac2de;
    --blue: #89b4fa;
    --green: #a6e3a1;
    --red: #f38ba8;
    --yellow: #f9e2af;
    --mauve: #cba6f7;
    --teal: #94e2d5;
  }

  :global(body) {
    margin: 0;
    padding: 0;
    background: var(--base);
    color: var(--text);
    font-family:
      'Segoe UI',
      system-ui,
      -apple-system,
      sans-serif;
    font-size: 14px;
    overflow: hidden;
    height: 100vh;
  }

  main {
    display: flex;
    flex-direction: column;
    height: 100vh;
    padding: 0;
  }

  /* Header */
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 20px;
    background: var(--mantle);
    border-bottom: 1px solid var(--surface0);
    -webkit-user-select: none;
    user-select: none;
  }

  header h1 {
    margin: 0;
    font-size: 20px;
    font-weight: 700;
    color: var(--blue);
  }

  .header-tools {
    display: flex;
    align-items: center;
    gap: 12px;
    flex-wrap: wrap;
    justify-content: flex-end;
  }

  .status-badges {
    display: flex;
    gap: 8px;
    align-items: center;
    flex-wrap: wrap;
  }

  .badge {
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 4px;
    font-weight: 500;
  }
  .badge.neutral {
    background: color-mix(in srgb, var(--surface1) 55%, transparent);
    color: var(--subtext1);
  }
  .badge.ok {
    background: color-mix(in srgb, var(--green) 20%, transparent);
    color: var(--green);
  }
  .badge.warn {
    background: color-mix(in srgb, var(--yellow) 20%, transparent);
    color: var(--yellow);
  }
  .badge.err {
    background: color-mix(in srgb, var(--red) 20%, transparent);
    color: var(--red);
  }

  .badge-button {
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 11px;
    font-weight: 600;
    white-space: nowrap;
    background: color-mix(in srgb, var(--blue) 20%, transparent);
    color: var(--blue);
  }

  .badge-button:hover:not(:disabled) {
    background: color-mix(in srgb, var(--blue) 30%, transparent);
  }

  .header-action {
    white-space: nowrap;
  }

  .startup-status {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 20px;
    border-bottom: 1px solid var(--surface0);
    background: color-mix(in srgb, var(--yellow) 10%, var(--base));
    color: var(--yellow);
    font-size: 12px;
  }

  .startup-status.error {
    background: color-mix(in srgb, var(--red) 10%, var(--base));
    color: var(--red);
  }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    margin: -1px;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
    white-space: nowrap;
    border: 0;
  }

  /* URL Bar */
  .url-bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 20px;
    background: var(--mantle);
    flex-wrap: wrap;
  }

  .url-bar input {
    flex: 1;
    min-width: 200px;
  }

  .error-text {
    color: var(--red);
    font-size: 12px;
  }

  /* Inputs */
  input[type='text'],
  select {
    background: var(--surface0);
    border: 1px solid var(--surface1);
    color: var(--text);
    padding: 8px 12px;
    border-radius: 6px;
    font-size: 13px;
    outline: none;
    transition: border-color 0.15s;
  }

  input[type='text']:focus {
    border-color: var(--blue);
  }

  button:focus-visible,
  input:focus-visible,
  select:focus-visible,
  [tabindex]:focus-visible {
    outline: 2px solid var(--blue);
    outline-offset: 2px;
  }

  input.input-error {
    border-color: var(--red);
  }

  select {
    padding: 6px 8px;
    cursor: pointer;
  }

  /* Buttons */
  button {
    padding: 8px 16px;
    border: none;
    border-radius: 6px;
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
    background: var(--surface0);
    color: var(--text);
    transition: background 0.15s;
  }

  button:hover:not(:disabled) {
    background: var(--surface1);
  }

  button:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  button.primary {
    background: var(--blue);
    color: var(--crust);
  }
  button.primary:hover:not(:disabled) {
    background: color-mix(in srgb, var(--blue) 85%, white);
  }

  button.danger {
    background: var(--red);
    color: var(--crust);
  }
  button.danger:hover:not(:disabled) {
    background: color-mix(in srgb, var(--red) 85%, white);
  }

  button.small {
    padding: 4px 10px;
    font-size: 12px;
  }

  /* Settings Row */
  .settings-row {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 10px 20px;
    background: var(--base);
    border-bottom: 1px solid var(--surface0);
    flex-wrap: wrap;
  }

  .setting {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .setting label {
    font-size: 12px;
    color: var(--subtext0);
    font-weight: 500;
    white-space: nowrap;
  }

  .output-dir {
    flex: 1;
    min-width: 200px;
  }

  .output-dir input {
    flex: 1;
    min-width: 120px;
  }

  .cookie-setting label {
    display: flex;
    align-items: center;
    gap: 4px;
    cursor: pointer;
  }

  /* Actions */
  .actions {
    display: flex;
    gap: 8px;
    padding: 10px 20px;
    flex-wrap: wrap;
  }

  /* Queue */
  .queue {
    flex: 1;
    overflow-y: auto;
    padding: 0;
  }

  .empty-state {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: var(--overlay0);
    font-size: 15px;
  }

  table {
    width: 100%;
    border-collapse: collapse;
    table-layout: fixed;
  }

  thead {
    position: sticky;
    top: 0;
    z-index: 1;
    background: var(--mantle);
  }

  th {
    padding: 8px 10px;
    text-align: left;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--subtext0);
    border-bottom: 1px solid var(--surface0);
  }

  td {
    padding: 8px 10px;
    border-bottom: 1px solid var(--surface0);
    vertical-align: middle;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .queue-item {
    height: 53px;
  }

  .virtual-spacer,
  .virtual-spacer:hover {
    background: transparent;
  }

  .virtual-spacer td {
    padding: 0;
    border: 0;
  }

  tr:hover {
    background: color-mix(in srgb, var(--surface0) 40%, transparent);
  }

  tr.downloading {
    background: color-mix(in srgb, var(--blue) 5%, transparent);
  }

  .col-check {
    width: 36px;
    text-align: center;
  }
  .col-title {
    width: auto;
  }
  .col-status {
    width: 180px;
  }
  .col-quality {
    width: 80px;
  }
  .col-format {
    width: 80px;
  }
  .col-progress {
    width: 130px;
  }
  .col-speed {
    width: 85px;
  }
  .col-eta {
    width: 65px;
  }
  .col-actions {
    width: 64px;
  }

  /* Title cell */
  .title-cell {
    display: flex;
    align-items: center;
    gap: 10px;
    overflow: hidden;
  }

  .thumb {
    width: 48px;
    height: 36px;
    object-fit: cover;
    border-radius: 4px;
    flex-shrink: 0;
  }

  .title-info {
    display: flex;
    flex-direction: column;
    overflow: hidden;
    min-width: 0;
  }

  .title-text {
    overflow: hidden;
    text-overflow: ellipsis;
    font-weight: 500;
  }

  .title-button {
    padding: 0;
    border: none;
    background: transparent;
    color: inherit;
    text-align: left;
    font: inherit;
    width: 100%;
    min-width: 0;
  }

  .title-button:hover:not(:disabled) {
    background: transparent;
    color: var(--blue);
  }

  .title-button .title-text {
    display: block;
    cursor: text;
  }

  .title-editor {
    width: 100%;
    min-width: 0;
    padding: 4px 6px;
    font-size: 13px;
    font-weight: 500;
    box-sizing: border-box;
  }

  .filename-error {
    color: var(--red);
    font-size: 10px;
    line-height: 1.25;
  }

  .channel {
    font-size: 11px;
    color: var(--subtext0);
  }

  .duration {
    font-size: 11px;
    color: var(--overlay0);
  }

  /* Status pills */
  .status-pill {
    display: inline-block;
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 11px;
    font-weight: 500;
    text-transform: capitalize;
  }
  .status-pill.fetching {
    background: color-mix(in srgb, var(--mauve) 20%, transparent);
    color: var(--mauve);
  }
  .status-pill.ready {
    background: color-mix(in srgb, var(--blue) 20%, transparent);
    color: var(--blue);
  }
  .status-pill.queued {
    background: color-mix(in srgb, var(--teal) 14%, transparent);
    color: var(--teal);
  }
  .status-pill.downloading {
    background: color-mix(in srgb, var(--teal) 20%, transparent);
    color: var(--teal);
  }
  .status-pill.postprocessing {
    background: color-mix(in srgb, var(--yellow) 20%, transparent);
    color: var(--yellow);
  }
  .status-pill.completed {
    background: color-mix(in srgb, var(--green) 20%, transparent);
    color: var(--green);
  }
  .status-pill.error {
    background: color-mix(in srgb, var(--red) 20%, transparent);
    color: var(--red);
  }
  .status-pill.cancelled {
    background: color-mix(in srgb, var(--overlay0) 20%, transparent);
    color: var(--overlay0);
  }

  .error-tooltip {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: var(--red);
    color: var(--crust);
    font-size: 10px;
    font-weight: 700;
    margin-left: 4px;
    padding: 0;
    cursor: help;
  }

  .error-summary {
    display: inline-block;
    max-width: 110px;
    margin-left: 4px;
    color: var(--red);
    font-size: 11px;
    vertical-align: middle;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .diagnostics-row:hover {
    background: transparent;
  }

  .diagnostics-panel {
    display: grid;
    gap: 8px;
    padding: 10px 12px;
    background: var(--mantle);
    border: 1px solid var(--surface0);
    border-radius: 6px;
  }

  .diagnostics-header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    color: var(--red);
    font-size: 12px;
    font-weight: 700;
  }

  .diagnostics-panel pre {
    margin: 0;
    max-height: 180px;
    overflow: auto;
    white-space: pre-wrap;
    color: var(--subtext1);
    font-family: ui-monospace, 'Cascadia Mono', Consolas, monospace;
    font-size: 11px;
    line-height: 1.45;
  }

  /* Progress bar */
  .progress-bar {
    position: relative;
    height: 20px;
    background: var(--surface0);
    border-radius: 4px;
    overflow: hidden;
  }

  .progress-fill {
    height: 100%;
    background: var(--blue);
    transition: width 0.3s ease;
    border-radius: 4px;
  }

  .progress-fill.convert {
    background: var(--yellow);
  }

  .progress-fill.complete {
    background: var(--green);
  }

  .progress-fill.error {
    background: var(--red);
  }

  .progress-text {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 11px;
    font-weight: 600;
    color: var(--text);
    text-shadow: 0 1px 2px rgba(0, 0, 0, 0.5);
  }

  .phase-progress {
    display: grid;
    gap: 3px;
  }

  .phase-progress-row {
    display: grid;
    grid-template-columns: 20px minmax(0, 1fr);
    align-items: center;
    gap: 5px;
  }

  .phase-progress-row .progress-bar {
    height: 12px;
  }

  .phase-progress-row .progress-text {
    font-size: 9px;
  }

  .phase-label {
    color: var(--subtext0);
    font-size: 9px;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-align: right;
  }

  .muted {
    color: var(--subtext0);
    font-size: 12px;
  }

  /* Inline selects in table */
  td select {
    width: 100%;
    padding: 3px 4px;
    font-size: 12px;
  }

  input[type='checkbox'] {
    accent-color: var(--blue);
    cursor: pointer;
  }

  /* Footer */
  footer {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 20px;
    background: var(--mantle);
    border-top: 1px solid var(--surface0);
    font-size: 12px;
    color: var(--subtext0);
    -webkit-user-select: none;
    user-select: none;
  }

  .sep {
    color: var(--surface2);
  }

  /* Scrollbar */
  .queue::-webkit-scrollbar {
    width: 8px;
  }
  .queue::-webkit-scrollbar-track {
    background: var(--base);
  }
  .queue::-webkit-scrollbar-thumb {
    background: var(--surface1);
    border-radius: 4px;
  }
  .queue::-webkit-scrollbar-thumb:hover {
    background: var(--surface2);
  }

  /* Cookie controls */
  .cookie-mode-select {
    min-width: 110px;
  }

  .cookie-browse {
    font-size: 12px;
    padding: 4px 10px;
    background: var(--surface0);
    color: var(--text);
    border: 1px solid var(--surface1);
    border-radius: 4px;
    cursor: pointer;
    max-width: 180px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cookie-browse:hover {
    background: var(--surface1);
  }

  .advanced-config {
    min-width: 220px;
  }

  .cookie-hint {
    font-size: 10px;
    color: var(--overlay0);
    font-style: italic;
  }

  .cookie-warn {
    display: none;
  }

  .cookie-warn-clean {
    font-size: 10px;
    color: var(--red);
    font-style: italic;
  }

  /* Playlist Modal */
  .modal-layer {
    position: fixed;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }

  .modal-backdrop {
    position: absolute;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    border: none;
    border-radius: 0;
    padding: 0;
  }

  .modal-backdrop:hover:not(:disabled),
  .modal-backdrop:focus-visible {
    background: rgba(0, 0, 0, 0.6);
    outline: none;
  }

  .modal {
    position: relative;
    z-index: 1;
    background: var(--base);
    border: 1px solid var(--surface1);
    border-radius: 12px;
    width: min(700px, 90vw);
    max-height: 80vh;
    display: flex;
    flex-direction: column;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4);
  }

  .modal-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    padding: 16px 20px 12px;
    border-bottom: 1px solid var(--surface0);
  }

  .modal-header h2 {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
    color: var(--text);
  }

  .modal-channel {
    font-size: 12px;
    color: var(--subtext0);
    margin-right: 8px;
  }

  .modal-count {
    font-size: 12px;
    color: var(--overlay0);
  }

  .modal-controls {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 20px;
    border-bottom: 1px solid var(--surface0);
  }

  .select-all-label {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    cursor: pointer;
    color: var(--subtext0);
    font-weight: 500;
  }

  .modal-list {
    flex: 1;
    overflow-y: auto;
    padding: 4px 0;
  }

  .modal-pagination {
    display: flex;
    align-items: center;
    justify-content: center;
    gap: 12px;
    padding: 8px 20px;
    border-top: 1px solid var(--surface0);
  }

  .playlist-entry {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 20px;
    cursor: pointer;
    transition: background 0.1s;
  }

  .playlist-entry:hover {
    background: color-mix(in srgb, var(--surface0) 50%, transparent);
  }

  .playlist-entry.entry-selected {
    background: color-mix(in srgb, var(--blue) 8%, transparent);
  }

  .entry-thumb {
    width: 64px;
    height: 36px;
    object-fit: cover;
    border-radius: 4px;
    flex-shrink: 0;
    background: var(--surface0);
  }

  .entry-info {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
  }

  .entry-title {
    font-size: 13px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .entry-duration {
    font-size: 11px;
    color: var(--overlay0);
  }

  .modal-footer {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
    padding: 12px 20px;
    border-top: 1px solid var(--surface0);
  }

  .update-modal {
    width: min(760px, 92vw);
  }

  .update-body {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 16px 20px;
    overflow-y: auto;
  }

  .update-meta {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: 10px 12px;
  }

  .update-meta-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 12px;
    padding: 10px 12px;
    background: var(--mantle);
    border: 1px solid var(--surface0);
    border-radius: 8px;
  }

  .update-meta-label {
    color: var(--subtext0);
    font-size: 12px;
    font-weight: 500;
  }

  .update-meta-value {
    color: var(--text);
    font-size: 12px;
    font-weight: 600;
    text-align: right;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  .update-summary {
    margin: 0;
    color: var(--subtext0);
    font-size: 13px;
    line-height: 1.5;
  }

  .update-progress-panel {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 12px;
    background: var(--mantle);
    border: 1px solid var(--surface0);
    border-radius: 8px;
  }

  .update-progress-header {
    display: flex;
    justify-content: space-between;
    gap: 12px;
    color: var(--subtext0);
    font-size: 12px;
  }

  .update-progress-bar {
    height: 12px;
    background: var(--surface0);
    border-radius: 999px;
    overflow: hidden;
  }

  .update-progress-fill {
    height: 100%;
    background: var(--blue);
    transition: width 0.2s ease;
  }

  .update-error {
    margin: 0;
    padding: 10px 12px;
    border-radius: 8px;
    background: color-mix(in srgb, var(--red) 12%, transparent);
    color: var(--red);
    font-size: 12px;
  }

  .update-notes-block {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }

  .update-notes-block h3 {
    margin: 0;
    font-size: 13px;
    font-weight: 600;
    color: var(--text);
  }

  .update-notes {
    max-height: 220px;
    overflow-y: auto;
    padding: 12px;
    background: var(--mantle);
    border: 1px solid var(--surface0);
    border-radius: 8px;
    color: var(--subtext0);
    font-size: 12px;
    line-height: 1.5;
    white-space: pre-wrap;
  }

  .modal-list::-webkit-scrollbar {
    width: 8px;
  }
  .modal-list::-webkit-scrollbar-track {
    background: var(--base);
  }
  .modal-list::-webkit-scrollbar-thumb {
    background: var(--surface1);
    border-radius: 4px;
  }
</style>
