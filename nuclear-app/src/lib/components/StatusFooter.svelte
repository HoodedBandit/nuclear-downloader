<script lang="ts">
  interface Props {
    counts: {
      total: number;
      ready: number;
      downloading: number;
      completed: number;
      failed: number;
    };
  }
  let { counts }: Props = $props();
</script>

<footer role="status" aria-live="polite">
  <span>{counts.total} items</span>
  <span class="sep">|</span>
  <span>{counts.ready} ready</span>
  <span class="sep">|</span>
  <span>{counts.downloading} downloading</span>
  <span class="sep">|</span>
  <span>{counts.completed} done</span>
  {#if counts.failed > 0}
    <span class="sep">|</span>
    <span class="error-text">{counts.failed} failed</span>
  {/if}
</footer>
