<script lang="ts">
  import Icon from './Icon.svelte';
  import { getPathBasename } from '$lib/settings-diagnostics-workflow';
  let {
    total,
    outputDir,
    browseOutputDir
  }: { total: number; outputDir: string; browseOutputDir: () => void } = $props();
</script>

<footer class="status-footer">
  <button class="footer-folder" onclick={browseOutputDir} title={outputDir}
    ><Icon name="folder" size={19} /><span
      >Save to <strong>{getPathBasename(outputDir) || 'Downloads'}</strong></span
    ></button
  ><span>{total} {total === 1 ? 'item' : 'items'}</span>
</footer>
