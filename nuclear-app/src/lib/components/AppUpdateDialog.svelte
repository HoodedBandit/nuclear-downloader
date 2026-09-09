<script lang="ts">
  import { accessibleDialog } from '$lib/accessible-dialog';
  import type { AppUpdateWorkflowState } from '$lib/app-update-workflow';

  interface Props {
    state: AppUpdateWorkflowState;
    maintenanceActive: boolean;
    updateBlockingWork: boolean;
    downloadPercent: number;
    formatByteCount: (bytes: number) => string;
    formatPublishedAt: (value: string | null) => string;
    onClose: () => void;
    onInstall: () => void | Promise<void>;
    onRefresh: () => Promise<boolean>;
  }

  let {
    state,
    maintenanceActive,
    updateBlockingWork,
    downloadPercent,
    formatByteCount,
    formatPublishedAt,
    onClose,
    onInstall,
    onRefresh
  }: Props = $props();
</script>

<div class="modal-layer">
  <button
    type="button"
    class="modal-backdrop"
    aria-label="Close update dialog"
    onclick={onClose}
    disabled={state.installRunning}
  ></button>
  <div
    class="modal update-modal"
    role="dialog"
    aria-modal="true"
    aria-labelledby="update-modal-title"
    tabindex="-1"
    use:accessibleDialog={{ onClose, locked: state.installRunning }}
  >
    <div class="modal-header">
      <div>
        <h2 id="update-modal-title">App Updates</h2>
        <span class="modal-count">GitHub Releases installer update</span>
      </div>
      <button
        class="small"
        onclick={onClose}
        disabled={state.installRunning}
        data-dialog-initial-focus
      >
        Close
      </button>
    </div>
    <div class="update-body">
      {#if state.checkState === 'checking' && !state.info}
        <p class="update-summary">Checking the latest stable GitHub Release...</p>
      {:else}
        <div class="update-meta">
          <div class="update-meta-row">
            <span class="update-meta-label">Current</span>
            <span class="update-meta-value"
              >v{state.appVersion ?? state.info?.currentVersion ?? 'Unknown'}</span
            >
          </div>
          <div class="update-meta-row">
            <span class="update-meta-label">Latest</span>
            <span class="update-meta-value">
              {#if state.info?.latestVersion}
                v{state.info.latestVersion}
              {:else}
                Unknown
              {/if}
            </span>
          </div>
          <div class="update-meta-row">
            <span class="update-meta-label">Published</span>
            <span class="update-meta-value">
              {formatPublishedAt(state.info?.publishedAt ?? null)}
            </span>
          </div>
          <div class="update-meta-row">
            <span class="update-meta-label">Installer</span>
            <span class="update-meta-value">
              {state.info?.installerName ?? 'Checked during install'}
            </span>
          </div>
        </div>

        {#if state.info?.hasUpdate}
          <p class="update-summary">
            A newer version is available. Installing it downloads the published Windows NSIS
            installer, closes the app, and relaunches Nuclear Downloader automatically.
          </p>
        {:else if state.info}
          <p class="update-summary">You are already on the latest stable release.</p>
        {/if}

        {#if state.installProgress}
          <div class="update-progress-panel">
            <div class="update-progress-header">
              <span>{state.installProgress.message ?? 'Working...'}</span>
              <span>
                {#if state.installProgress.totalBytes}
                  {formatByteCount(state.installProgress.downloadedBytes)} / {formatByteCount(
                    state.installProgress.totalBytes
                  )}
                {:else if state.installProgress.downloadedBytes > 0}
                  {formatByteCount(state.installProgress.downloadedBytes)}
                {:else}
                  Waiting...
                {/if}
              </span>
            </div>
            <div class="update-progress-bar">
              <div class="update-progress-fill" style="width: {downloadPercent}%"></div>
            </div>
          </div>
        {/if}

        {#if state.error}
          <p class="update-error" role="alert" aria-live="assertive">{state.error}</p>
        {/if}

        <div class="update-notes-block">
          <h3>Release Notes</h3>
          <div class="update-notes">
            {state.info?.notes ?? 'No release notes were provided for this release.'}
          </div>
        </div>
      {/if}
    </div>
    <div class="modal-footer">
      {#if state.info?.hasUpdate && state.info.latestVersion}
        <button
          class="primary"
          onclick={onInstall}
          disabled={maintenanceActive || state.checkState === 'checking' || updateBlockingWork}
          title={updateBlockingWork ? 'Finish or cancel queued downloads first' : ''}
        >
          {#if state.installRunning}
            Installing...
          {:else}
            Install v{state.info.latestVersion}
          {/if}
        </button>
      {/if}
      <button onclick={onRefresh} disabled={state.checkState === 'checking' || maintenanceActive}>
        {state.checkState === 'checking' ? 'Checking...' : 'Refresh Check'}
      </button>
    </div>
  </div>
</div>
