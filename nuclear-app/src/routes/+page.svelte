<script lang="ts">
  import AppUpdateDialog from '$lib/components/AppUpdateDialog.svelte';
  import PlaylistDialog from '$lib/components/PlaylistDialog.svelte';
  import HeaderRuntime from '$lib/components/HeaderRuntime.svelte';
  import SettingsRow from '$lib/components/SettingsRow.svelte';
  import UrlBar from '$lib/components/UrlBar.svelte';
  import QueueToolbar from '$lib/components/QueueToolbar.svelte';
  import QueueTable from '$lib/components/QueueTable.svelte';
  import '$lib/styles/app.css';
  import StatusFooter from '$lib/components/StatusFooter.svelte';
  import { getVersion } from '@tauri-apps/api/app';
  import { open, save } from '@tauri-apps/plugin-dialog';
  import { onMount, tick } from 'svelte';
  import { AppStateController } from '$lib/app-state-controller';
  import { isTerminalOperation } from '$lib/backend-state';
  import type { AppSnapshot } from '$lib/bindings/AppSnapshot';
  import type { OperationSnapshot } from '$lib/bindings/OperationSnapshot';
  import type { StateDelta } from '$lib/bindings/StateDelta';
  import { normalizeAppError } from '$lib/frontend-errors';
  import { type QueueItem, type OutputFormat } from '$lib/frontend-types';
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
    createSettingsDiagnosticsState
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
  <HeaderRuntime
    {appUpdateState}
    {runtimeState}
    {maintenanceActive}
    backendDraining={queueState.backendSnapshot?.draining ?? false}
    {runtimeBadgeClass}
    {runtimeBadgeTitle}
    {runtimeBadgeText}
    {hasUpdateBlockingWork}
    {updateDownloaderRuntime}
    {openUpdateModal}
    {refreshDownloaderRuntime}
    {handleManualUpdateCheck}
  />

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
  <UrlBar
    bind:urlInput={inspectionState.urlInput}
    playlistLoading={inspectionState.playlistLoading}
    urlError={inspectionState.urlError}
    {maintenanceActive}
    {canStartDownloads}
    {runtimeState}
    runtimeUpdatePercent={() => runtimeWorkflow.getUpdatePercent()}
    {handleUrlSubmit}
    {cancelInspection}
  />

  <!-- Settings Row -->
  <SettingsRow
    bind:globalQuality={settingsState.globalQuality}
    bind:globalFormat={settingsState.globalFormat}
    bind:outputDir={settingsState.outputDir}
    bind:useCookies={settingsState.useCookies}
    bind:cookieMode={settingsState.cookieMode}
    bind:cookieBrowser={settingsState.cookieBrowser}
    bind:compatConfigPath={settingsState.compatConfigPath}
    outputDirError={settingsState.outputDirError}
    cookieFilePath={settingsState.cookieFilePath}
    {applyGlobalQuality}
    {applyGlobalFormat}
    {browseOutputDir}
    {browseCookieFile}
    {browseCompatConfigFile}
  />

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
  <PlaylistDialog
    modal={inspectionState.playlistModal}
    page={inspectionState.playlistPage}
    pageCount={getPlaylistPageCount()}
    visibleEntries={getVisiblePlaylistEntries()}
    selectionState={playlistSelectionState}
    {formatDuration}
    onClose={closePlaylistModal}
    onToggleAll={(checked) => inspectionWorkflow.toggleAll(checked)}
    onToggleEntry={(index, checked) => {
      if (inspectionState.playlistModal)
        inspectionState.playlistModal.entries[index].selected = checked;
    }}
    onChangePage={changePlaylistPage}
    onAddSelection={addPlaylistSelection}
  />
{/if}

<!-- Update Modal -->
{#if appUpdateState.modalOpen}
  <AppUpdateDialog
    state={appUpdateState}
    {maintenanceActive}
    updateBlockingWork={hasUpdateBlockingWork()}
    downloadPercent={appUpdateWorkflow.downloadPercent()}
    {formatByteCount}
    {formatPublishedAt}
    onClose={closeUpdateModal}
    onInstall={installAppUpdate}
    onRefresh={handleManualUpdateCheck}
  />
{/if}
