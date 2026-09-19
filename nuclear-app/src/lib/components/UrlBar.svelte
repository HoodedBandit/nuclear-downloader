<script lang="ts">
  import Icon from './Icon.svelte';
  import type { RuntimeWorkflowState } from '$lib/runtime-workflow';

  let {
    urlInput = $bindable(),
    playlistLoading,
    maintenanceActive,
    canStartDownloads,
    urlError,
    runtimeState,
    handleUrlSubmit,
    cancelInspection
  }: {
    urlInput: string;
    playlistLoading: boolean;
    maintenanceActive: boolean;
    canStartDownloads: boolean;
    urlError: string;
    runtimeState: RuntimeWorkflowState;
    handleUrlSubmit: (event: SubmitEvent) => void;
    cancelInspection: () => void | Promise<void>;
  } = $props();
</script>

<form class="url-bar" autocomplete="off" onsubmit={handleUrlSubmit}>
  <label class="sr-only" for="video-url">Video or playlist URL</label>
  <div class="url-input-wrap">
    <Icon name="link" size={22} />
    <input
      id="video-url"
      type="text"
      name="nuclear-source-url"
      placeholder="Paste a video or playlist link"
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
  </div>
  <button type="submit" class="primary" disabled={!canStartDownloads || playlistLoading}>
    {playlistLoading ? 'Reading link…' : 'Add link'}
  </button>
  {#if playlistLoading}
    <button onclick={cancelInspection}>Cancel</button>
  {/if}
  {#if urlError}<span id="url-error" class="sr-only"
      >This link could not be read. Check Settings for more information.</span
    >{/if}
  {#if runtimeState.updateRunning}
    <span class="muted" role="status" aria-live="polite"> Updating download tools… </span>
  {/if}
</form>
