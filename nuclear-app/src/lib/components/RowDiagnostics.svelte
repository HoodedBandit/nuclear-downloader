<script lang="ts">
  import type { QueueItem } from '$lib/frontend-types';
  import { redactDiagnosticText } from '$lib/queue-logic';

  let {
    item,
    copyDiagnostics
  }: {
    item: QueueItem;
    copyDiagnostics: (item: QueueItem) => void | Promise<void>;
  } = $props();
</script>

<tr class="diagnostics-row">
  <td colspan="9">
    <div class="diagnostics-panel">
      <div class="diagnostics-header">
        <span>{item.errorCode ?? 'download_failed'}</span>
        <button class="small" onclick={() => copyDiagnostics(item)}>Copy Diagnostics</button>
      </div>
      <pre>{redactDiagnosticText(item.errorDetail ?? item.error ?? '')}</pre>
    </div>
  </td>
</tr>
