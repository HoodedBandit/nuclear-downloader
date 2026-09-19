<script lang="ts">
  import Icon from './Icon.svelte';
  import { accessibleDialog } from '$lib/accessible-dialog';
  let { onClose }: { onClose: () => void } = $props();
</script>

<div class="modal-layer">
  <button class="modal-backdrop" aria-label="Close help" onclick={onClose}></button>
  <div
    class="modal help-dialog"
    role="dialog"
    aria-modal="true"
    aria-labelledby="help-title"
    tabindex="-1"
    use:accessibleDialog={{ onClose: onClose }}
  >
    <div class="modal-header">
      <div>
        <h2 id="help-title">A little help</h2>
        <p class="modal-subtitle">From link to file, in a few steps.</p>
      </div>
      <button
        class="icon-button"
        aria-label="Close help"
        data-dialog-initial-focus
        onclick={onClose}><Icon name="close" /></button
      >
    </div>
    <div class="help-body">
      <ol>
        <li>
          <strong>Add a link.</strong>
          <p>
            Paste a video, audio, or playlist link. For playlists, choose the items you want to
            keep.
          </p>
        </li>
        <li>
          <strong>Make it yours.</strong>
          <p>
            Choose a format, quality, and destination. Click a queued title to rename the file
            before downloading.
          </p>
        </li>
        <li>
          <strong>Download.</strong>
          <p>
            Start the queue or select individual items. Stop and retry from each row; completed
            downloads can be shown in their folder.
          </p>
        </li>
      </ol>
      <p class="help-note">
        Settings contains appearance, optional cookies, download tools, updates, and diagnostics.
        Existing files in your destination are preserved.
      </p>
    </div>
  </div>
</div>
