<script lang="ts">
  import { accessibleDialog } from './accessible-dialog';

  let open = $state(false);

  function close(): void {
    open = false;
  }
</script>

<main>
  <button type="button" onclick={() => (open = true)}>Open dialog</button>
</main>

{#if open}
  <div
    role="dialog"
    aria-modal="true"
    aria-labelledby="harness-title"
    tabindex="-1"
    use:accessibleDialog={{ onClose: close }}
  >
    <h2 id="harness-title">Test dialog</h2>
    <button type="button" data-dialog-initial-focus onclick={close}>Close dialog</button>
    <button type="button">Last action</button>
  </div>
{/if}
