<script lang="ts">
  import type { RuntimeWorkflowState } from '$lib/runtime-workflow';

  let {
    urlInput = $bindable(),
    playlistLoading,
    maintenanceActive,
    canStartDownloads,
    urlError,
    runtimeState,
    runtimeUpdatePercent,
    handleUrlSubmit,
    cancelInspection
  }: {
    urlInput: string;
    playlistLoading: boolean;
    maintenanceActive: boolean;
    canStartDownloads: boolean;
    urlError: string;
    runtimeState: RuntimeWorkflowState;
    runtimeUpdatePercent: () => number;
    handleUrlSubmit: (event: SubmitEvent) => void;
    cancelInspection: () => void | Promise<void>;
  } = $props();
</script>

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
  {#if runtimeState.error}
    <span class="error-text" role="alert" aria-live="assertive">{runtimeState.error}</span>
  {:else if runtimeState.status?.message && runtimeState.status.state !== 'ready'}
    <span class="error-text">{runtimeState.status.message}</span>
  {/if}
  {#if runtimeState.updateProgress}
    <span class="muted" role="status" aria-live="polite">
      {runtimeState.updateProgress.message ?? 'Runtime update'}
      {Math.round(runtimeUpdatePercent())}%
    </span>
  {/if}
</form>
