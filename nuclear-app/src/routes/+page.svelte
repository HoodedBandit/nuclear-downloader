<script lang="ts">
  import { getVersion } from '@tauri-apps/api/app';
  import { open, save } from '@tauri-apps/plugin-dialog';
  import { onMount, tick } from 'svelte';
  import { accessibleDialog } from '$lib/accessible-dialog';
  import { AppStateController } from '$lib/app-state-controller';
  import { isTerminalOperation } from '$lib/backend-state';
  import type { AppSnapshot } from '$lib/bindings/AppSnapshot';
  import type { OperationSnapshot } from '$lib/bindings/OperationSnapshot';
  import type { StateDelta } from '$lib/bindings/StateDelta';
  import { normalizeAppError } from '$lib/frontend-errors';
  import {
    supportedBrowsers,
    videoFormats,
    audioFormats,
    type QueueItem,
    type OutputFormat
  } from '$lib/frontend-types';
  import { invokeCommand as invoke, listenEvent as listen } from '$lib/ipc-client';
  import { OperationWaitRegistry } from '$lib/operation-wait-registry';
  import { PageLifetime } from '$lib/page-lifetime';
  import {
    QueuePresentationController,
    QUEUE_ROW_HEIGHT_PX,
    createQueuePresentationState,
    isEditablePendingStatus,
    canEditFilename,
    canRetryItem,
    getQueueItemDisplayTitle,
    shouldShowConversionProgress,
    getStatusLabel,
    roundedProgress,
    formatDuration
  } from '$lib/queue-presentation';
  import { QueueActionsController, createQueueActionState } from '$lib/queue-actions';
  import { InspectionWorkflow, createInspectionState } from '$lib/inspection-workflow';
  import { RuntimeWorkflowController, createRuntimeWorkflowState } from '$lib/runtime-workflow';
  import {
    AppUpdateWorkflowController,
    createAppUpdateWorkflowState,
    formatPublishedAt
  } from '$lib/app-update-workflow';
  import {
    SettingsDiagnosticsWorkflow,
    createSettingsDiagnosticsState,
    getPathBasename
  } from '$lib/settings-diagnostics-workflow';
  import {
    canStartWork,
    deriveSelectionState,
    isAudioOnlyFormat,
    isUpdateBlockingStatus,
    resolveAvailableFormat,
    redactDiagnosticText
  } from '$lib/queue-logic';
  import {
    createStartupSubsystems,
    deriveStartupState,
    type StartupSubsystem,
    type StartupSubsystemState
  } from '$lib/startup-state';

  const queueState = $state(createQueuePresentationState());
  const settingsState = $state(createSettingsDiagnosticsState());
  const inspectionState = $state(createInspectionState());
  const queueActionState = $state(createQueueActionState());
  const runtimeState = $state(createRuntimeWorkflowState());
  const appUpdateState = $state(createAppUpdateWorkflowState());

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
  let titleEditorInput = $state<HTMLInputElement | null>(null);
  let startupSubsystems = $state(createStartupSubsystems());
  let startupIssues = $state<string[]>([]);
  let queueSelectAll = $state<HTMLInputElement | null>(null);
  let playlistSelectAll = $state<HTMLInputElement | null>(null);
  let queueViewport = $state<HTMLElement | null>(null);
  let backendStateError = $state<string | null>(null);
  let persistenceHealthError = $state<string | null>(null);
  const operationWaiters = new OperationWaitRegistry();
  const pageLifetime = new PageLifetime((error) => globalThis.reportError(error));
  const rendererUnloadError = new Error('Renderer was unloaded before the operation completed.');
  const appStateController = new AppStateController(
    () => invoke('get_app_snapshot'),
    pageLifetime.guard(applyBackendSnapshot),
    pageLifetime.guard(handleBackendStateError)
  );

  const commands = {
    invoke,
    isActive: () => pageLifetime.isActive,
    unloadedError: rendererUnloadError
  };
  const operationCommands = { ...commands, waitForOperation };
  const queuePresentation = new QueuePresentationController(queueState, {
    ...commands,
    focusFilenameEditor: async () => {
      await tick();
      if (!pageLifetime.isActive) return;
      titleEditorInput?.focus();
      titleEditorInput?.select();
    },
    clearFilenameEditor: () => {
      titleEditorInput = null;
    }
  });
  const queueActions = new QueueActionsController(queueActionState, {
    ...commands,
    getItems: () => queuePresentation.getItems(),
    replaceItem: (id, mapper) => queuePresentation.replaceItem(id, mapper),
    getCanStartDownloads: () => canStartDownloads,
    getGlobalQuality: () => settingsState.globalQuality,
    getGlobalFormat: () => settingsState.globalFormat,
    clearProgressDisplayState: (id) => queuePresentation.clearProgressDisplayState(id),
    getEditingTitleId: () => queueState.editing.itemId,
    cancelFilenameEdit: () => queuePresentation.cancelFilenameEdit(),
    reloadAppSnapshot
  });
  const settingsWorkflow = new SettingsDiagnosticsWorkflow(settingsState, {
    commands,
    ui: {
      dialogs: { open, save },
      confirm: (message) => window.confirm(message),
      clipboard: { writeText: (text) => navigator.clipboard.writeText(text) }
    },
    queue: {
      getItems: () => queueState.items,
      updateQueueItemSettings: (item, input) => queueActions.updateQueueItemSettings(item, input)
    },
    startup: { setSubsystem: setStartupSubsystem, reportIssue: reportStartupIssue },
    getRuntimeStatus: () => runtimeState.status,
    getQueueItemDisplayTitle
  });
  const runtimeWorkflow = new RuntimeWorkflowController(runtimeState, {
    ...operationCommands,
    appUpdateRunning: () => appUpdateState.installRunning,
    hasUpdateBlockingWork,
    backendReadiness: () => queueState.backendSnapshot?.runtimeReadiness ?? null,
    setStartupSubsystem: (state) => setStartupSubsystem('runtime', state),
    reportStartupIssue
  });
  const appUpdateWorkflow = new AppUpdateWorkflowController(appUpdateState, {
    ...operationCommands,
    getVersion,
    runtimeUpdateRunning: () => runtimeState.updateRunning,
    hasUpdateBlockingWork,
    setAppVersionStartup: (state) => setStartupSubsystem('appVersion', state),
    setUpdateCheckStartup: (state) => setStartupSubsystem('updateCheck', state),
    reportStartupIssue,
    appendStartupIssue: (message) => {
      startupIssues = [...startupIssues, message];
    }
  });
  const inspectionWorkflow = new InspectionWorkflow(inspectionState, {
    ...operationCommands,
    getSettings: () => ({
      canStartDownloads,
      startupState,
      runtimeCanDownload: runtimeWorkflow.canDownload(),
      outputDirError: settingsState.outputDirError,
      globalFormat: settingsState.globalFormat,
      globalQuality: settingsState.globalQuality,
      outputDir: settingsState.outputDir
    }),
    readCookie: () => settingsWorkflow.getCookieConfigSnapshot(),
    readCompat: () => settingsWorkflow.getCompatConfigSnapshot(),
    queue: {
      getItems: () => queuePresentation.getItems(),
      retainMetadata: (url, metadata) => queuePresentation.retainMetadata(url, metadata)
    },
    setQueueActionError: (message) => {
      queueActionState.queueActionError = message;
    }
  });

  // -- Lifecycle --
  onMount(() => {
    let stateListening = false;
    pageLifetime.own(() => appStateController.stop());
    pageLifetime.own(() => operationWaiters.dispose(rendererUnloadError));
    const queueResizeObserver =
      typeof ResizeObserver === 'undefined'
        ? undefined
        : new ResizeObserver(
            pageLifetime.guard(([entry]) => {
              if (entry) queueState.viewport.height = entry.contentRect.height;
            })
          );
    if (queueResizeObserver) pageLifetime.own(() => queueResizeObserver.disconnect());
    if (queueViewport) {
      queueState.viewport.height = queueViewport.clientHeight || queueState.viewport.height;
      queueResizeObserver?.observe(queueViewport);
    }

    const setup = async () => {
      const listenerErrors: string[] = [];

      try {
        await appStateController.start(
          (handler) =>
            listen(
              'app-state-changed',
              pageLifetime.guard((event) => handler(event.payload))
            ),
          (handler) =>
            listen(
              'app-state-resync-required',
              pageLifetime.guard((event) => handler(event.payload))
            )
        );
        stateListening = true;
      } catch (error) {
        if (!pageLifetime.isActive) return;
        listenerErrors.push(`App state: ${normalizeAppError(error)}`);
      }
      if (!pageLifetime.isActive) return;

      try {
        const unlisten = await listen(
          'download-progress',
          pageLifetime.guard((event) => {
            queuePresentation.applyProgress(event.payload);
          })
        );
        pageLifetime.own(unlisten);
      } catch (error) {
        if (!pageLifetime.isActive) return;
        listenerErrors.push(`Download progress: ${normalizeAppError(error)}`);
      }
      if (!pageLifetime.isActive) return;

      try {
        const unlisten = await listen(
          'update-install-progress',
          pageLifetime.guard((event) => {
            appUpdateWorkflow.applyProgress(event.payload);
          })
        );
        pageLifetime.own(unlisten);
      } catch (error) {
        if (!pageLifetime.isActive) return;
        listenerErrors.push(`App update progress: ${normalizeAppError(error)}`);
      }
      if (!pageLifetime.isActive) return;

      try {
        const unlisten = await listen(
          'downloader-runtime-update-progress',
          pageLifetime.guard((event) => {
            runtimeWorkflow.applyProgress(event.payload);
          })
        );
        pageLifetime.own(unlisten);
      } catch (error) {
        if (!pageLifetime.isActive) return;
        listenerErrors.push(`Runtime update progress: ${normalizeAppError(error)}`);
      }
      if (!pageLifetime.isActive) return;

      if (!stateListening || backendStateError) {
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
      pageLifetime.dispose();
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
      if (!pageLifetime.isActive) return;
      handleBackendStateError(error);
      throw error;
    }
  }

  function applyBackendSnapshot(snapshot: AppSnapshot, delta?: StateDelta): void {
    queuePresentation.applySnapshot(snapshot, delta);
    backendStateError = null;
    persistenceHealthError = snapshot.persistenceHealth.degraded
      ? `Queue history is not being saved. ${normalizeAppError(snapshot.persistenceHealth.error ?? 'Persistence is degraded.')}`
      : null;
    operationWaiters.settle(snapshot.operations);

    runtimeState.updateRunning = snapshot.operations.some(
      (operation) => operation.kind === 'runtime_update' && !isTerminalOperation(operation)
    );
    appUpdateState.installRunning = snapshot.operations.some(
      (operation) => operation.kind === 'app_update' && !isTerminalOperation(operation)
    );
  }

  function waitForOperation(
    operationId: string,
    timeoutMs = 35 * 60 * 1000
  ): Promise<OperationSnapshot> {
    return operationWaiters.waitWithRefresh(
      operationId,
      () => queueState.backendSnapshot?.operations ?? [],
      timeoutMs,
      reloadAppSnapshot
    );
  }

  function hasUpdateBlockingWork(): boolean {
    return queueState.items.some((item) => isUpdateBlockingStatus(item.status));
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

  function handleQueueSelectionChange(event: Event): void {
    const checked = (event.currentTarget as HTMLInputElement).checked;
    queuePresentation.setAllSelected(checked);
  }

  function handleQueueScroll(event: Event): void {
    queueState.viewport.scrollTop = (event.currentTarget as HTMLElement).scrollTop;
  }

  function handlePlaylistSelectionToggle(event: Event): void {
    const checked = (event.currentTarget as HTMLInputElement).checked;
    inspectionWorkflow.toggleAll(checked);
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
  let queueSummary = $derived(queuePresentation.summary());
  let startupState = $derived(deriveStartupState(startupSubsystems));
  let maintenanceActive = $derived(
    queueState.backendSnapshot?.maintenanceActive ??
      (runtimeState.updateRunning || appUpdateState.installRunning)
  );
  let canStartDownloads = $derived(
    canStartWork({
      runtimeReady:
        runtimeWorkflow.canDownload() &&
        startupState !== 'error' &&
        queueState.backendSnapshot !== null &&
        !queueState.backendSnapshot.draining,
      outputDirectoryReady: settingsState.outputDirValidated,
      maintenanceActive
    })
  );
  let queueSelectionState = $derived(queuePresentation.selectionState());
  let queueWindow = $derived(queuePresentation.window());
  let playlistSelectionState = $derived(
    deriveSelectionState(inspectionState.playlistModal?.entries ?? [])
  );

  $effect(() => {
    const maxScrollTop = Math.max(
      0,
      queueState.items.length * QUEUE_ROW_HEIGHT_PX - queueState.viewport.height
    );
    if (queueState.viewport.scrollTop > maxScrollTop) {
      queueState.viewport.scrollTop = maxScrollTop;
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

  const initializeAppVersion = () => appUpdateWorkflow.initializeAppVersion();
  const initializeRuntime = () => runtimeWorkflow.initialize();
  const initializeOutputDirectory = () => settingsWorkflow.initializeOutputDirectory();
  const initializeUpdateCheck = () => appUpdateWorkflow.initializeUpdateCheck();
  const runtimeBadgeClass = () => runtimeWorkflow.badgeClass();
  const runtimeBadgeText = () => runtimeWorkflow.badgeText();
  const runtimeBadgeTitle = () => runtimeWorkflow.badgeTitle();
  const refreshDownloaderRuntime = () => runtimeWorkflow.refresh();
  const updateDownloaderRuntime = () => runtimeWorkflow.update();
  const browseCookieFile = () => settingsWorkflow.browseCookieFile();
  const browseCompatConfigFile = () => settingsWorkflow.browseCompatConfigFile();
  const browseOutputDir = () => settingsWorkflow.browseOutputDir();
  const exportDiagnostics = () => settingsWorkflow.exportDiagnostics();
  const clearDiagnostics = () => settingsWorkflow.clearDiagnostics();
  const openUpdateModal = () => appUpdateWorkflow.openModal();
  const closeUpdateModal = () => appUpdateWorkflow.closeModal();
  const handleManualUpdateCheck = () =>
    appUpdateWorkflow.check({ openModal: true, showErrors: true });
  const installAppUpdate = () => appUpdateWorkflow.install();
  const addToQueue = () => inspectionWorkflow.addToQueue();
  const cancelInspection = () => inspectionWorkflow.cancelInspection();
  const addPlaylistSelection = () => inspectionWorkflow.addPlaylistSelection();
  const closePlaylistModal = () => inspectionWorkflow.closePlaylist();
  const getPlaylistPageCount = () => inspectionWorkflow.pageCount();
  const getVisiblePlaylistEntries = () => inspectionWorkflow.visibleEntries();
  const downloadAll = () => queueActions.downloadAll();
  const downloadSelected = () => queueActions.downloadSelected();
  const cancelAll = () => queueActions.cancelAll();
  const removeSelected = () => queueActions.removeSelected();
  const clearCompleted = () => queueActions.clearCompleted();
  const applyGlobalQuality = () => queueActions.applyGlobalQuality();
  const applyGlobalFormat = () => queueActions.applyGlobalFormat();
  const cancelFilenameEdit = () => queuePresentation.cancelFilenameEdit();
  const downloadItem = (item: QueueItem) => queueActions.downloadItem(item);
  const cancelItem = (item: QueueItem) => queueActions.cancelItem(item);
  const retryItem = (item: QueueItem) => queueActions.retryItem(item);
  const updateQueueItemSettings = (
    item: QueueItem,
    input: Parameters<QueueActionsController['updateQueueItemSettings']>[1]
  ) => queueActions.updateQueueItemSettings(item, input);
  const beginFilenameEdit = (item: QueueItem) => queuePresentation.beginFilenameEdit(item);
  const commitFilenameEdit = (id?: string | null) => queuePresentation.commitFilenameEdit(id);
  const toggleDiagnostics = (id: string) => queuePresentation.toggleDiagnostics(id);
  const copyDiagnostics = (item: QueueItem) => settingsWorkflow.copyDiagnostics(item);
  const changePlaylistPage = (delta: number) => inspectionWorkflow.changePage(delta);
</script>

<main>
  <!-- Header -->
  <header>
    <h1>Nuclear Downloader</h1>
    <div class="header-tools">
      <div class="status-badges">
        {#if appUpdateState.appVersion}
          <span class="badge neutral">v{appUpdateState.appVersion}</span>
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
            {queueState.backendSnapshot?.draining ? 'Cancelling work…' : 'Maintenance active'}
          </span>
        {/if}
        {#if runtimeState.updateCheck?.updateAvailable}
          <button
            type="button"
            class="badge-button"
            onclick={updateDownloaderRuntime}
            disabled={maintenanceActive || hasUpdateBlockingWork()}
            title={hasUpdateBlockingWork()
              ? 'Finish or cancel queued downloads first'
              : (runtimeState.updateCheck.message ?? '')}
          >
            {runtimeState.updateRunning ? 'Runtime...' : 'Update Runtime'}
          </button>
        {/if}
        {#if appUpdateState.info?.hasUpdate && appUpdateState.info.latestVersion}
          <button
            type="button"
            class="badge-button"
            onclick={openUpdateModal}
            disabled={appUpdateState.checkState === 'checking' || maintenanceActive}
          >
            Update v{appUpdateState.info.latestVersion}
          </button>
        {/if}
      </div>
      <button
        class="small header-action"
        onclick={refreshDownloaderRuntime}
        disabled={runtimeState.checkState === 'checking' || maintenanceActive}
      >
        {runtimeState.checkState === 'checking' ? 'Runtime...' : 'Check Runtime'}
      </button>
      <button
        class="small header-action"
        onclick={handleManualUpdateCheck}
        disabled={appUpdateState.checkState === 'checking' || maintenanceActive}
      >
        {#if appUpdateState.installRunning}
          Installing...
        {:else if appUpdateState.checkState === 'checking'}
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
      bind:value={inspectionState.urlInput}
      autocomplete="off"
      autocapitalize="none"
      spellcheck={false}
      inputmode="url"
      aria-autocomplete="none"
      disabled={inspectionState.playlistLoading || maintenanceActive}
      class:input-error={Boolean(inspectionState.urlError)}
      aria-describedby={inspectionState.urlError ? 'url-error' : undefined}
    />
    <button
      type="submit"
      class="primary"
      disabled={!canStartDownloads || inspectionState.playlistLoading}
    >
      {inspectionState.playlistLoading ? 'Loading...' : 'Add'}
    </button>
    {#if inspectionState.playlistLoading}
      <button onclick={cancelInspection}>Cancel</button>
    {/if}
    {#if inspectionState.urlError}
      <span id="url-error" class="error-text" role="alert" aria-live="assertive"
        >{inspectionState.urlError}</span
      >
    {/if}
    {#if runtimeState.error}
      <span class="error-text" role="alert" aria-live="assertive">{runtimeState.error}</span>
    {:else if runtimeState.status?.message && runtimeState.status.state !== 'ready'}
      <span class="error-text">{runtimeState.status.message}</span>
    {/if}
    {#if runtimeState.updateProgress}
      <span class="muted" role="status" aria-live="polite">
        {runtimeState.updateProgress.message ?? 'Runtime update'}
        {Math.round(runtimeWorkflow.getUpdatePercent())}%
      </span>
    {/if}
  </form>

  <!-- Settings Row -->
  <section class="settings-row">
    <div class="setting">
      <label for="quality">Quality</label>
      <select id="quality" bind:value={settingsState.globalQuality} onchange={applyGlobalQuality}>
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
      <select id="format" bind:value={settingsState.globalFormat} onchange={applyGlobalFormat}>
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
      <input id="outdir" type="text" bind:value={settingsState.outputDir} readonly />
      <button onclick={browseOutputDir}>Browse</button>
      {#if settingsState.outputDirError}
        <span class="error-text" role="alert">{settingsState.outputDirError}</span>
      {/if}
    </div>
    <div class="setting cookie-setting">
      <label>
        <input type="checkbox" bind:checked={settingsState.useCookies} />
        Cookies
      </label>
      {#if settingsState.useCookies}
        <label class="sr-only" for="cookie-mode">Cookie source</label>
        <select id="cookie-mode" bind:value={settingsState.cookieMode} class="cookie-mode-select">
          <option value="browser">From Browser</option>
          <option value="file">From File</option>
        </select>
        {#if settingsState.cookieMode === 'browser'}
          <label class="sr-only" for="cookie-browser">Cookie browser</label>
          <select id="cookie-browser" bind:value={settingsState.cookieBrowser}>
            {#each supportedBrowsers as b (b)}
              <option value={b}>{b.charAt(0).toUpperCase() + b.slice(1)}</option>
            {/each}
          </select>
          {#if settingsState.cookieBrowser === 'chrome' || settingsState.cookieBrowser === 'edge' || settingsState.cookieBrowser === 'brave' || settingsState.cookieBrowser === 'chromium'}
            <span class="cookie-warn"
              >Chromium browsers block cookie access — use Firefox or a cookie file instead</span
            >
          {:else}
            <span class="cookie-hint"
              >Close {settingsState.cookieBrowser} first if errors occur</span
            >
          {/if}
        {:else}
          <button class="cookie-browse" onclick={browseCookieFile}>
            {settingsState.cookieFilePath
              ? settingsState.cookieFilePath.split(/[\\/]/).pop()
              : 'Select cookies.txt'}
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
        {settingsState.compatConfigPath ? getPathBasename(settingsState.compatConfigPath) : 'None'}
      </button>
      {#if settingsState.compatConfigPath}
        <button class="small" onclick={() => (settingsState.compatConfigPath = '')}>Clear</button>
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
    {#if queueActionState.cancelAllError}
      <span class="error-text" role="alert" aria-live="assertive"
        >{queueActionState.cancelAllError}</span
      >
    {/if}
    {#if queueActionState.queueActionError}
      <span class="error-text" role="alert" aria-live="assertive"
        >{queueActionState.queueActionError}</span
      >
    {/if}
    {#if persistenceHealthError}
      <span class="error-text" role="alert" aria-live="assertive">{persistenceHealthError}</span>
    {/if}
    {#if settingsState.diagnosticsError}
      <span class="error-text" role="alert" aria-live="assertive"
        >{settingsState.diagnosticsError}</span
      >
    {:else if settingsState.diagnosticsMessage}
      <span class="muted" role="status" aria-live="polite">{settingsState.diagnosticsMessage}</span>
    {/if}
  </section>

  <!-- Queue Table -->
  <section
    class="queue"
    bind:this={queueViewport}
    onscroll={handleQueueScroll}
    data-queue-count={queueState.items.length}
  >
    {#if queueState.items.length === 0}
      <div class="empty-state">
        <p>No videos in queue. Paste a video URL above to get started.</p>
      </div>
    {:else}
      <table aria-rowcount={queueState.items.length + 1}>
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
          {#if queueWindow.topSpacerHeight > 0}
            <tr class="virtual-spacer" aria-hidden="true">
              <td colspan="9" style={`height: ${queueWindow.topSpacerHeight}px`}></td>
            </tr>
          {/if}
          {#each queueWindow.rows as row (row.item.id)}
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
                  bind:checked={queueState.items[i].selected}
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
                    {#if queueState.editing.itemId === item.id}
                      <input
                        bind:this={titleEditorInput}
                        bind:value={queueState.editing.draft}
                        type="text"
                        class="title-editor"
                        aria-label="Edit queued filename"
                        onblur={() => commitFilenameEdit(item.id)}
                        onkeydown={handleFilenameEditorKeydown}
                        onclick={(event) => event.stopPropagation()}
                      />
                      {#if queueState.editing.error}
                        <span class="filename-error" role="alert">{queueState.editing.error}</span>
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
          {#if queueWindow.bottomSpacerHeight > 0}
            <tr class="virtual-spacer" aria-hidden="true">
              <td colspan="9" style={`height: ${queueWindow.bottomSpacerHeight}px`}></td>
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
{#if inspectionState.playlistModal}
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
          <h2 id="playlist-modal-title">{inspectionState.playlistModal.info.title}</h2>
          {#if inspectionState.playlistModal.info.channel}
            <span class="modal-channel">{inspectionState.playlistModal.info.channel}</span>
          {/if}
          <span class="modal-count">
            {inspectionState.playlistModal.info.truncated
              ? `Showing first ${inspectionState.playlistModal.info.entry_count} videos`
              : `${inspectionState.playlistModal.info.entry_count} videos`}
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
          {inspectionState.playlistModal.entries.filter((entry) => entry.selected).length} of {inspectionState
            .playlistModal.entries.length} selected
        </span>
      </div>
      <div class="modal-list">
        {#each getVisiblePlaylistEntries() as row (row.entry.url)}
          {@const entry = row.entry}
          <label class="playlist-entry" class:entry-selected={entry.selected}>
            <input
              type="checkbox"
              bind:checked={inspectionState.playlistModal.entries[row.index].selected}
            />
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
          <button
            class="small"
            onclick={() => changePlaylistPage(-1)}
            disabled={inspectionState.playlistPage === 0}>Previous</button
          >
          <span class="muted"
            >Page {inspectionState.playlistPage + 1} of {getPlaylistPageCount()}</span
          >
          <button
            class="small"
            onclick={() => changePlaylistPage(1)}
            disabled={inspectionState.playlistPage >= getPlaylistPageCount() - 1}>Next</button
          >
        </div>
      {/if}
      <div class="modal-footer">
        <button
          class="primary"
          onclick={addPlaylistSelection}
          disabled={!inspectionState.playlistModal.entries.some((entry) => entry.selected)}
        >
          Add {inspectionState.playlistModal.entries.filter((entry) => entry.selected).length} Videos
          to Queue
        </button>
        <button onclick={closePlaylistModal}>Cancel</button>
      </div>
    </div>
  </div>
{/if}

<!-- Update Modal -->
{#if appUpdateState.modalOpen}
  <div class="modal-layer">
    <button
      type="button"
      class="modal-backdrop"
      aria-label="Close update dialog"
      onclick={closeUpdateModal}
      disabled={appUpdateState.installRunning}
    ></button>
    <div
      class="modal update-modal"
      role="dialog"
      aria-modal="true"
      aria-labelledby="update-modal-title"
      tabindex="-1"
      use:accessibleDialog={{ onClose: closeUpdateModal, locked: appUpdateState.installRunning }}
    >
      <div class="modal-header">
        <div>
          <h2 id="update-modal-title">App Updates</h2>
          <span class="modal-count">GitHub Releases installer update</span>
        </div>
        <button
          class="small"
          onclick={closeUpdateModal}
          disabled={appUpdateState.installRunning}
          data-dialog-initial-focus
        >
          Close
        </button>
      </div>
      <div class="update-body">
        {#if appUpdateState.checkState === 'checking' && !appUpdateState.info}
          <p class="update-summary">Checking the latest stable GitHub Release...</p>
        {:else}
          <div class="update-meta">
            <div class="update-meta-row">
              <span class="update-meta-label">Current</span>
              <span class="update-meta-value"
                >v{appUpdateState.appVersion ??
                  appUpdateState.info?.currentVersion ??
                  'Unknown'}</span
              >
            </div>
            <div class="update-meta-row">
              <span class="update-meta-label">Latest</span>
              <span class="update-meta-value">
                {#if appUpdateState.info?.latestVersion}
                  v{appUpdateState.info.latestVersion}
                {:else}
                  Unknown
                {/if}
              </span>
            </div>
            <div class="update-meta-row">
              <span class="update-meta-label">Published</span>
              <span class="update-meta-value">
                {formatPublishedAt(appUpdateState.info?.publishedAt ?? null)}
              </span>
            </div>
            <div class="update-meta-row">
              <span class="update-meta-label">Installer</span>
              <span class="update-meta-value">
                {appUpdateState.info?.installerName ?? 'Checked during install'}
              </span>
            </div>
          </div>

          {#if appUpdateState.info?.hasUpdate}
            <p class="update-summary">
              A newer version is available. Installing it downloads the published Windows NSIS
              installer, closes the app, and relaunches Nuclear Downloader automatically.
            </p>
          {:else if appUpdateState.info}
            <p class="update-summary">You are already on the latest stable release.</p>
          {/if}

          {#if appUpdateState.installProgress}
            <div class="update-progress-panel">
              <div class="update-progress-header">
                <span>{appUpdateState.installProgress.message ?? 'Working...'}</span>
                <span>
                  {#if appUpdateState.installProgress.totalBytes}
                    {formatByteCount(appUpdateState.installProgress.downloadedBytes)} / {formatByteCount(
                      appUpdateState.installProgress.totalBytes
                    )}
                  {:else if appUpdateState.installProgress.downloadedBytes > 0}
                    {formatByteCount(appUpdateState.installProgress.downloadedBytes)}
                  {:else}
                    Waiting...
                  {/if}
                </span>
              </div>
              <div class="update-progress-bar">
                <div
                  class="update-progress-fill"
                  style="width: {appUpdateWorkflow.downloadPercent()}%"
                ></div>
              </div>
            </div>
          {/if}

          {#if appUpdateState.error}
            <p class="update-error" role="alert" aria-live="assertive">{appUpdateState.error}</p>
          {/if}

          <div class="update-notes-block">
            <h3>Release Notes</h3>
            <div class="update-notes">
              {appUpdateState.info?.notes ?? 'No release notes were provided for this release.'}
            </div>
          </div>
        {/if}
      </div>
      <div class="modal-footer">
        {#if appUpdateState.info?.hasUpdate && appUpdateState.info.latestVersion}
          <button
            class="primary"
            onclick={installAppUpdate}
            disabled={maintenanceActive ||
              appUpdateState.checkState === 'checking' ||
              hasUpdateBlockingWork()}
            title={hasUpdateBlockingWork() ? 'Finish or cancel queued downloads first' : ''}
          >
            {#if appUpdateState.installRunning}
              Installing...
            {:else}
              Install v{appUpdateState.info.latestVersion}
            {/if}
          </button>
        {/if}
        <button
          onclick={handleManualUpdateCheck}
          disabled={appUpdateState.checkState === 'checking' || maintenanceActive}
        >
          {appUpdateState.checkState === 'checking' ? 'Checking...' : 'Refresh Check'}
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
