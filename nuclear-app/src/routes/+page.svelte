<script lang="ts">
  import QueueToolbar from '$lib/components/QueueToolbar.svelte';
  import QueueTable from '$lib/components/QueueTable.svelte';
  import '$lib/styles/app.css';
  import StatusFooter from '$lib/components/StatusFooter.svelte';
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
    getQueueItemDisplayTitle,
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
    isUpdateBlockingStatus,
    resolveAvailableFormat
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
    pageLifetime.own(() => queuePresentation.dispose());
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
  <QueueToolbar
    summary={queueSummary}
    {canStartDownloads}
    cancelAllError={queueActionState.cancelAllError}
    queueActionError={queueActionState.queueActionError}
    {persistenceHealthError}
    diagnosticsError={settingsState.diagnosticsError}
    diagnosticsMessage={settingsState.diagnosticsMessage}
    {downloadAll}
    {downloadSelected}
    {removeSelected}
    {clearCompleted}
    {cancelAll}
    {exportDiagnostics}
    {clearDiagnostics}
  />

  <!-- Queue Table -->
  <QueueTable
    state={queueState}
    window={queueWindow}
    selectionState={queueSelectionState}
    {canStartDownloads}
    bind:viewport={queueViewport}
    bind:selectAll={queueSelectAll}
    bind:titleEditorInput
    onScroll={handleQueueScroll}
    onSelectionChange={handleQueueSelectionChange}
    setSelected={(itemId, selected) => queuePresentation.setSelected(itemId, selected)}
    setDraft={(draft) => (queueState.editing.draft = draft)}
    {beginFilenameEdit}
    {commitFilenameEdit}
    {handleFilenameEditorKeydown}
    {toggleDiagnostics}
    changeQuality={handleItemQualityChange}
    changeFormat={handleItemFormatChange}
    {downloadItem}
    {cancelItem}
    {retryItem}
    {copyDiagnostics}
  />

  <!-- Status Bar -->
  <StatusFooter counts={queueSummary.counts} />
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
