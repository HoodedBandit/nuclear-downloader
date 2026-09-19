<script lang="ts">
  import type { QueueItem } from '$lib/frontend-types';
  import type { QueueRowActions, FilenameEditorActions } from '$lib/queue-row-actions';
  import type { queueViewWindow } from '$lib/queue-view';
  import QueueRow from './QueueRow.svelte';
  let {
    window,
    canStartDownloads,
    viewport = $bindable(),
    onScroll,
    actions,
    editor,
    visibleItems,
    filtered = false
  }: {
    window: ReturnType<typeof queueViewWindow>;
    canStartDownloads: boolean;
    viewport: HTMLElement | null;
    onScroll: (event: Event & { currentTarget: EventTarget & HTMLElement }) => void;
    actions: QueueRowActions;
    editor: FilenameEditorActions;
    visibleItems: QueueItem[];
    filtered?: boolean;
  } = $props();
</script>

<section
  class="queue"
  bind:this={viewport}
  onscroll={onScroll}
  data-queue-count={visibleItems.length}
>
  {#if visibleItems.length === 0}
    <div class="empty-state">
      <span class="empty-illustration" aria-hidden="true">↓</span>
      <h2>{filtered ? 'Nothing here just yet' : 'Your downloads, all in one place'}</h2>
      <p>
        {filtered
          ? 'Try another view or change your search.'
          : 'Paste a video or playlist link above to get started.'}
      </p>
    </div>
  {:else}
    <table aria-label="Download queue" aria-rowcount={visibleItems.length + 1}>
      <thead class="sr-only">
        <tr>
          <th class="col-check">Selection</th>
          <th class="col-title">Title</th>
          <th>Format and quality</th>
          <th>Status and progress</th>
          <th>Actions</th>
        </tr>
      </thead>
      <tbody>
        {#if window.topSpacerHeight > 0}
          <tr class="virtual-spacer" aria-hidden="true">
            <td colspan="5" style={`height: ${window.topSpacerHeight}px`}></td>
          </tr>
        {/if}
        {#each window.rows as row (row.item.id)}
          <QueueRow
            item={row.item}
            index={row.index}
            selected={row.item.selected}
            {canStartDownloads}
            {actions}
            {editor}
          />
        {/each}
        {#if window.bottomSpacerHeight > 0}
          <tr class="virtual-spacer" aria-hidden="true">
            <td colspan="5" style={`height: ${window.bottomSpacerHeight}px`}></td>
          </tr>
        {/if}
      </tbody>
    </table>
  {/if}
</section>
