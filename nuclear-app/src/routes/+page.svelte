<script lang="ts">
  import Sidebar from '$lib/components/Sidebar.svelte';
  import SettingsDialog from '$lib/components/SettingsDialog.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import { filterLabels } from '$lib/queue-view';
  import { applyTheme, type ThemePreference } from '$lib/theme';
  import { AppearanceController, createAppearanceState } from '$lib/appearance-controller';
  import { collectInterfaceErrors } from '$lib/interface-errors';
  import AppUpdateDialog from '$lib/components/AppUpdateDialog.svelte';
  import PlaylistDialog from '$lib/components/PlaylistDialog.svelte';
  import DownloadDefaults from '$lib/components/DownloadDefaults.svelte';
  import DownloadAccessSettings from '$lib/components/DownloadAccessSettings.svelte';
  import HelpDialog from '$lib/components/HelpDialog.svelte';
  import DiagnosticsSettings from '$lib/components/DiagnosticsSettings.svelte';
  import RuntimeSettings from '$lib/components/RuntimeSettings.svelte';
  import UrlBar from '$lib/components/UrlBar.svelte';
  import QueueToolbar from '$lib/components/QueueToolbar.svelte';
  import QueueTable from '$lib/components/QueueTable.svelte';
  import '$lib/styles/app.css';
  import StatusFooter from '$lib/components/StatusFooter.svelte';
  import { getVersion } from '@tauri-apps/api/app';
  import { open, save } from '@tauri-apps/plugin-dialog';
  import { onMount, tick, untrack } from 'svelte';
  import { createErrorInbox, syncErrors } from '$lib/error-inbox';
  import { AppSessionController, createAppSessionState } from '$lib/app-session';
  import { QueueViewController, createQueueViewState } from '$lib/queue-view-controller';
  import { FilenameEditorController, createFilenameEditorState } from '$lib/filename-editor';
  import { InterfaceErrorReporter } from '$lib/ui-error-reporter';
  import { PageLifetime } from '$lib/page-lifetime';
  import type { QueueRowActions } from '$lib/queue-row-actions';
  import { type QueueItem, type OutputFormat } from '$lib/frontend-types';
  import { invokeCommand as invoke, listenEvent as listen } from '$lib/ipc-client';
  import {
    QueuePresentationController,
    createQueuePresentationState,
    formatDuration,
    formatByteCount
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
  import { deriveStartupState } from '$lib/startup-state';

  const queueState = $state(createQueuePresentationState());
  const settingsState = $state(createSettingsDiagnosticsState());
  const inspectionState = $state(createInspectionState());
  const queueActionState = $state(createQueueActionState());
  const runtimeState = $state(createRuntimeWorkflowState());
  const appUpdateState = $state(createAppUpdateWorkflowState());

  let settingsOpen = $state(false);
  let helpOpen = $state(false);
  let searchInput = $state<HTMLInputElement | null>(null);
  let queueViewport = $state<HTMLElement | null>(null);
  const errorInbox = $state(createErrorInbox());
  const sessionState = $state(createAppSessionState());
  const viewState = $state(createQueueViewState());
  const filenameState = $state(createFilenameEditorState());
  const lifetime = new PageLifetime((error) => globalThis.reportError(error));
  const errors = new InterfaceErrorReporter(
    errorInbox,
    () => settingsOpen,
    () => lifetime.isActive
  );
  const queuePresentation = new QueuePresentationController(queueState, {
    isActive: () => lifetime.isActive
  });
  const session = new AppSessionController(sessionState, {
    lifetime,
    invoke,
    listen,
    errors,
    queue: queuePresentation,
    runtime: runtimeState,
    appUpdate: appUpdateState
  });
  const queueView = new QueueViewController(viewState, () => queueState.items);
  const commands = {
    invoke,
    isActive: () => session.isActive,
    unloadedError: session.unloadedError
  };
  const operationCommands = {
    ...commands,
    waitForOperation: (id: string, timeout?: number) => session.waitForOperation(id, timeout)
  };
  const filenameEditor = new FilenameEditorController(filenameState, {
    ...commands,
    errors,
    getItems: () => queueState.items,
    save: (itemId, filenameOverride) =>
      invoke('update_queue_item', { itemId, input: { filenameOverride } })
  });
  const queueActions = new QueueActionsController(queueActionState, {
    ...commands,
    getItems: () => queuePresentation.getItems(),
    replaceItem: (id, mapper) => queuePresentation.replaceItem(id, mapper),
    getCanStartDownloads: () => canStartDownloads,
    getGlobalQuality: () => settingsState.globalQuality,
    getGlobalFormat: () => settingsState.globalFormat,
    clearProgressDisplayState: (id) => queuePresentation.clearProgressDisplayState(id),
    getEditingTitleId: () => filenameState.itemId,
    cancelFilenameEdit: () => filenameEditor.cancel(),
    flushFilenameEdits: (ids) => filenameEditor.flushFor(ids),
    reloadAppSnapshot: () => session.reload()
  });
  const settingsWorkflow = new SettingsDiagnosticsWorkflow(settingsState, {
    commands,
    errors,
    ui: {
      dialogs: { open, save },
      confirm: (message) => window.confirm(message)
    },
    queue: {
      getItems: () => queueState.items,
      updateQueueItemSettings: (item, input) => queueActions.updateQueueItemSettings(item, input)
    },
    startup: { setSubsystem: (name, state) => session.setSubsystem(name, state) }
  });
  const runtimeWorkflow = new RuntimeWorkflowController(runtimeState, {
    ...operationCommands,
    errors,
    appUpdateRunning: () => appUpdateState.installRunning,
    hasUpdateBlockingWork,
    backendReadiness: () => queueState.backendSnapshot?.runtimeReadiness ?? null,
    setStartupSubsystem: (state) => session.setSubsystem('runtime', state)
  });
  const appUpdateWorkflow = new AppUpdateWorkflowController(appUpdateState, {
    ...operationCommands,
    errors,
    getVersion,
    runtimeUpdateRunning: () => runtimeState.updateRunning,
    hasUpdateBlockingWork,
    setAppVersionStartup: (state) => session.setSubsystem('appVersion', state),
    setUpdateCheckStartup: (state) => session.setSubsystem('updateCheck', state)
  });
  const inspectionWorkflow = new InspectionWorkflow(inspectionState, {
    ...operationCommands,
    getSettings: () => ({
      ...settingsState,
      canStartDownloads,
      startupState,
      runtimeCanDownload: runtimeWorkflow.canDownload()
    }),
    readCookie: () => settingsWorkflow.getCookieConfigSnapshot(),
    readCompat: () => settingsWorkflow.getCompatConfigSnapshot(),
    queue: {
      getItems: () => queuePresentation.getItems(),
      retainMetadata: (url, metadata, selection) =>
        queuePresentation.retainMetadata(url, metadata, selection),
      retainPlaylistMetadata: (entries) => queuePresentation.retainPlaylistMetadata(entries)
    },
    setQueueActionError: (message) => {
      queueActionState.queueActionError = message;
    }
  });

  const filter = $derived(viewState.filter);
  const searchState = viewState.search;
  const appearanceState = $state(createAppearanceState());
  const appearance = new AppearanceController(appearanceState, () => session.isActive);
  const visibleItems = $derived(queueView.visibleItems());
  const filterCounts = $derived(queueView.counts());
  const unreadErrors = $derived(errorInbox.entries.filter((entry) => !entry.read).length);
  const errorSources = $derived(
    collectInterfaceErrors({
      inspectionState,
      queueActionState,
      runtimeState,
      themeError: appearanceState.error,
      queueState
    })
  );
  const hasCurrentError = $derived(
    errorSources.some((source) => Boolean(source.detail)) ||
      Object.keys(errorInbox.reportedActive).length > 0
  );
  $effect(() => {
    const sources = errorSources;
    const opened = settingsOpen;
    untrack(() => syncErrors(errorInbox, sources, opened));
  });
  function openSettings(): void {
    settingsOpen = true;
  }
  const selectedCount = $derived(queueView.selectedCount());

  const resetQueueScroll = () => queueView.resetScroll();
  const changeFilter = (next: typeof viewState.filter) => queueView.changeFilter(next);
  const selectVisible = () => queueView.selectVisible();
  const toggleSearch = () => queueView.toggleSearch(() => tick().then(() => searchInput?.focus()));
  const changeTheme = (next: ThemePreference) => appearance.change(next);
  $effect(() => {
    applyTheme(appearanceState.theme, appearanceState.systemDark);
  });

  onMount(() => {
    session.own(appearance.start());
    if (queueViewport) session.own(queueView.attach(queueViewport));
    void session.start({
      runtime: runtimeWorkflow,
      appUpdate: appUpdateWorkflow,
      initializeOutputDirectory: () => settingsWorkflow.initializeOutputDirectory()
    });
    return () => session.dispose();
  });

  function hasUpdateBlockingWork(): boolean {
    return queueState.items.some((item) => isUpdateBlockingStatus(item.status));
  }

  function handleUrlSubmit(event: SubmitEvent): void {
    event.preventDefault();
    void addToQueue();
  }
  let queueSummary = $derived(queuePresentation.summary());
  let startupState = $derived(deriveStartupState(sessionState.subsystems));
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
  let queueSelectionState = $derived(queueView.selection());
  let queueWindow = $derived(queueView.window());
  let playlistSelectionState = $derived(
    deriveSelectionState(inspectionState.playlistModal?.entries ?? [])
  );

  $effect(() => {
    void visibleItems.length;
    void viewState.viewport.height;
    queueView.clampScroll();
  });

  const openUpdateModal = () => {
    settingsOpen = false;
    appUpdateWorkflow.openModal();
  };
  const closeUpdateModal = () => {
    appUpdateWorkflow.closeModal();
    settingsOpen = true;
  };
  const handleManualUpdateCheck = () => {
    settingsOpen = false;
    return appUpdateWorkflow.check({ openModal: true, showErrors: true });
  };
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
  const rowActions: QueueRowActions = {
    select: (id, selected) => queueView.setSelected(id, selected),
    details: () => openSettings(),
    quality: (item, quality) => queueActions.updateQueueItemSettings(item, { quality }),
    format: (item, value) =>
      queueActions.updateQueueItemSettings(item, {
        format: resolveAvailableFormat(value as OutputFormat, item.hasAudio, 'mp4')
      }),
    download: (item) => queueActions.downloadItem(item),
    cancel: (item) => queueActions.cancelItem(item),
    retry: (item) => queueActions.retryItem(item),
    reveal: (item) => queueActions.revealDownload(item)
  };
  const editorActions = {
    state: filenameState,
    begin: (item: QueueItem) => filenameEditor.begin(item),
    commit: (id: string) => filenameEditor.commit(id),
    cancel: () => filenameEditor.cancel(),
    setDraft: (draft: string) => filenameEditor.setDraft(draft)
  };
  const browseOutputDir = () => settingsWorkflow.browseOutputDir();
  const changePlaylistPage = (delta: number) => inspectionWorkflow.changePage(delta);
</script>

<main class="app-shell">
  <Sidebar
    {filter}
    counts={filterCounts}
    onFilter={changeFilter}
    onSettings={openSettings}
    {unreadErrors}
    onHelp={() => (helpOpen = true)}
  />
  <section class="workspace" aria-label="Downloads workspace">
    <div class="workspace-content">
      <header class="workspace-header">
        <div>
          <h1>{filterLabels[filter]}</h1>
          <p>
            {filterCounts.active} in progress <span aria-hidden="true">·</span>
            {filterCounts.queued} queued
          </p>
        </div>
        <div class="header-actions">
          <button
            class="icon-button"
            aria-label={searchState.open ? 'Close search' : 'Search downloads'}
            aria-expanded={searchState.open}
            onclick={toggleSearch}
            ><Icon name={searchState.open ? 'close' : 'search'} size={22} /></button
          ><button
            class="icon-button"
            aria-label="Open settings"
            title="Settings"
            onclick={openSettings}><Icon name="more" size={24} /></button
          >
        </div>
      </header>
      {#if searchState.open}<div class="search-bar">
          <Icon name="search" size={18} /><input
            bind:this={searchInput}
            bind:value={searchState.query}
            oninput={resetQueueScroll}
            aria-label="Search downloads"
            placeholder="Search titles, channels, or links"
            onkeydown={(event) => {
              if (event.key === 'Escape') void toggleSearch();
            }}
          />{#if searchState.query}<button
              class="text-button"
              onclick={() => {
                searchState.query = '';
                resetQueueScroll();
              }}>Clear</button
            >{/if}
        </div>{/if}
      {#if hasCurrentError}
        <div class="inline-alert error-notice" role="alert">
          <Icon name="alert" size={18} /><span
            >An error occurred. Check Settings for more information.</span
          ><button class="text-button" onclick={openSettings}>Open Settings</button>
        </div>
      {/if}
      <UrlBar
        bind:urlInput={inspectionState.urlInput}
        playlistLoading={inspectionState.playlistLoading}
        urlError={inspectionState.urlError}
        {maintenanceActive}
        {canStartDownloads}
        {runtimeState}
        {handleUrlSubmit}
        {cancelInspection}
      />
      <DownloadDefaults
        state={settingsState}
        onQuality={applyGlobalQuality}
        onFormat={applyGlobalFormat}
        {browseOutputDir}
      />
      <QueueToolbar
        summary={queueSummary}
        {canStartDownloads}
        {downloadAll}
        {downloadSelected}
        {removeSelected}
        {clearCompleted}
        {cancelAll}
        selectAll={selectVisible}
        selectionState={queueSelectionState}
        {selectedCount}
      />
      <QueueTable
        window={queueWindow}
        {canStartDownloads}
        bind:viewport={queueViewport}
        onScroll={(event) => queueView.scroll(event.currentTarget.scrollTop)}
        actions={rowActions}
        editor={editorActions}
        {visibleItems}
        filtered={filter !== 'all' || Boolean(searchState.query.trim())}
      />
    </div>
    <StatusFooter
      total={queueState.items.length}
      outputDir={settingsState.outputDir}
      {browseOutputDir}
    />
  </section>
</main>

{#if settingsOpen}
  <SettingsDialog
    errors={errorInbox.entries}
    theme={appearanceState.theme}
    saving={appearanceState.saving}
    error={appearanceState.error}
    onTheme={changeTheme}
    onClose={() => (settingsOpen = false)}
  >
    {#snippet advanced()}<DownloadAccessSettings
        state={settingsState}
        browseCookies={() => settingsWorkflow.browseCookieFile()}
        browseConfig={() => settingsWorkflow.browseCompatConfigFile()}
      />{/snippet}
    {#snippet runtime()}
      <RuntimeSettings
        {appUpdateState}
        {runtimeState}
        workflow={runtimeWorkflow}
        {maintenanceActive}
        backendDraining={queueState.backendSnapshot?.draining ?? false}
        {hasUpdateBlockingWork}
        {openUpdateModal}
        {handleManualUpdateCheck}
      />
    {/snippet}
    {#snippet diagnostics()}<DiagnosticsSettings
        state={settingsState}
        workflow={settingsWorkflow}
      />{/snippet}
  </SettingsDialog>
{/if}
{#if helpOpen}<HelpDialog onClose={() => (helpOpen = false)} />{/if}

<!-- Playlist Picker Modal -->
{#if inspectionState.playlistModal}
  <PlaylistDialog
    modal={inspectionState.playlistModal}
    page={inspectionState.playlistPage}
    pageCount={getPlaylistPageCount()}
    visibleEntries={getVisiblePlaylistEntries()}
    selectionState={playlistSelectionState}
    loading={inspectionState.playlistLoading}
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
