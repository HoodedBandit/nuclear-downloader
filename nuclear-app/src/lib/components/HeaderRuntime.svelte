<script lang="ts">
  import type { AppUpdateWorkflowState } from '$lib/app-update-workflow';
  import type { RuntimeWorkflowState } from '$lib/runtime-workflow';

  let {
    appUpdateState,
    runtimeState,
    maintenanceActive,
    backendDraining,
    runtimeBadgeClass,
    runtimeBadgeTitle,
    runtimeBadgeText,
    hasUpdateBlockingWork,
    updateDownloaderRuntime,
    openUpdateModal,
    refreshDownloaderRuntime,
    handleManualUpdateCheck
  }: {
    appUpdateState: AppUpdateWorkflowState;
    runtimeState: RuntimeWorkflowState;
    maintenanceActive: boolean;
    backendDraining: boolean;
    runtimeBadgeClass: () => string;
    runtimeBadgeTitle: () => string;
    runtimeBadgeText: () => string;
    hasUpdateBlockingWork: () => boolean;
    updateDownloaderRuntime: () => void | Promise<void>;
    openUpdateModal: () => void;
    refreshDownloaderRuntime: () => void | Promise<void>;
    handleManualUpdateCheck: () => Promise<boolean>;
  } = $props();
</script>

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
          {backendDraining ? 'Cancelling work…' : 'Maintenance active'}
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
