<script lang="ts">
  import type {
    SettingsDiagnosticsState,
    SettingsDiagnosticsWorkflow
  } from '$lib/settings-diagnostics-workflow';
  let {
    state,
    workflow
  }: { state: SettingsDiagnosticsState; workflow: SettingsDiagnosticsWorkflow } = $props();
</script>

<div class="diagnostic-actions">
  <button onclick={() => workflow.exportDiagnostics()}>Export diagnostics</button>
  <button onclick={() => workflow.clearDiagnostics()}>Clear diagnostics</button>
</div>
{#if state.diagnosticsError}<p class="error-text" role="alert">{state.diagnosticsError}</p>
{:else if state.diagnosticsMessage}<p class="muted" role="status">
    {state.diagnosticsMessage}
  </p>{/if}
