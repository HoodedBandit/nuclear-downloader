<script lang="ts">
  interface Props {
    summary: {
      hasReady: boolean;
      hasSelectedReady: boolean;
      hasSelected: boolean;
      hasCompleted: boolean;
      hasActive: boolean;
    };
    canStartDownloads: boolean;
    cancelAllError: string | null;
    queueActionError: string | null;
    persistenceHealthError: string | null;
    diagnosticsError: string | null;
    diagnosticsMessage: string | null;
    downloadAll: () => void;
    downloadSelected: () => void;
    removeSelected: () => void;
    clearCompleted: () => void;
    cancelAll: () => void;
    exportDiagnostics: () => void;
    clearDiagnostics: () => void;
  }
  let {
    summary,
    canStartDownloads,
    cancelAllError,
    queueActionError,
    persistenceHealthError,
    diagnosticsError,
    diagnosticsMessage,
    downloadAll,
    downloadSelected,
    removeSelected,
    clearCompleted,
    cancelAll,
    exportDiagnostics,
    clearDiagnostics
  }: Props = $props();
</script>

<section class="actions">
  <button class="primary" onclick={downloadAll} disabled={!canStartDownloads || !summary.hasReady}
    >Download All</button
  >
  <button onclick={downloadSelected} disabled={!canStartDownloads || !summary.hasSelectedReady}
    >Download Selected</button
  >
  <button onclick={removeSelected} disabled={!summary.hasSelected}>Remove Selected</button>
  <button onclick={clearCompleted} disabled={!summary.hasCompleted}>Clear Done</button>
  <button class="danger" onclick={cancelAll} disabled={!summary.hasActive}>Cancel All</button>
  <button onclick={exportDiagnostics}>Export Diagnostics</button>
  <button onclick={clearDiagnostics}>Clear Diagnostics</button>
  {#if cancelAllError}
    <span class="error-text" role="alert" aria-live="assertive">{cancelAllError}</span>
  {/if}
  {#if queueActionError}
    <span class="error-text" role="alert" aria-live="assertive">{queueActionError}</span>
  {/if}
  {#if persistenceHealthError}
    <span class="error-text" role="alert" aria-live="assertive">{persistenceHealthError}</span>
  {/if}
  {#if diagnosticsError}
    <span class="error-text" role="alert" aria-live="assertive">{diagnosticsError}</span>
  {:else if diagnosticsMessage}
    <span class="muted" role="status" aria-live="polite">{diagnosticsMessage}</span>
  {/if}
</section>
