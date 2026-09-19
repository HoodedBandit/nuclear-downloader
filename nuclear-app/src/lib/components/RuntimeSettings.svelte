<script lang="ts">
  import HeaderRuntime from './HeaderRuntime.svelte';
  import type { RuntimeWorkflowController, RuntimeWorkflowState } from '$lib/runtime-workflow';
  import type { AppUpdateWorkflowState } from '$lib/app-update-workflow';
  let {
    appUpdateState,
    runtimeState,
    workflow,
    maintenanceActive,
    backendDraining,
    hasUpdateBlockingWork,
    openUpdateModal,
    handleManualUpdateCheck
  }: {
    appUpdateState: AppUpdateWorkflowState;
    runtimeState: RuntimeWorkflowState;
    workflow: RuntimeWorkflowController;
    maintenanceActive: boolean;
    backendDraining: boolean;
    hasUpdateBlockingWork: () => boolean;
    openUpdateModal: () => void;
    handleManualUpdateCheck: () => Promise<boolean>;
  } = $props();
</script>

<HeaderRuntime
  {appUpdateState}
  {runtimeState}
  {maintenanceActive}
  {backendDraining}
  {hasUpdateBlockingWork}
  runtimeBadgeClass={() => workflow.badgeClass()}
  runtimeBadgeTitle={() => workflow.badgeTitle()}
  runtimeBadgeText={() => workflow.badgeText()}
  updateDownloaderRuntime={() => workflow.update()}
  refreshDownloaderRuntime={() => workflow.refresh()}
  {openUpdateModal}
  {handleManualUpdateCheck}
/>
{#if runtimeState.error}<p class="error-text" role="alert">{runtimeState.error}</p>{/if}
{#if runtimeState.updateProgress}<p class="muted" role="status">
    {runtimeState.updateProgress.message}
    {#if runtimeState.updateRunning}{Math.round(workflow.getUpdatePercent())}%{/if}
  </p>{/if}
