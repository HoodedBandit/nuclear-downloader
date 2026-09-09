<script lang="ts">
  import type { QueueItem } from '$lib/frontend-types';
  import type { QueuePresentationState } from '$lib/queue-presentation';
  import type { deriveSelectionState } from '$lib/queue-logic';
  import QueueRow from './QueueRow.svelte';

  type QueueWindow = {
    rows: Array<{ item: QueueItem; index: number }>;
    topSpacerHeight: number;
    bottomSpacerHeight: number;
  };

  let {
    state,
    window,
    selectionState,
    canStartDownloads,
    viewport = $bindable(),
    selectAll = $bindable(),
    titleEditorInput = $bindable(),
    onScroll,
    onSelectionChange,
    setSelected,
    setDraft,
    beginFilenameEdit,
    commitFilenameEdit,
    handleFilenameEditorKeydown,
    toggleDiagnostics,
    changeQuality,
    changeFormat,
    downloadItem,
    cancelItem,
    retryItem,
    copyDiagnostics
  }: {
    state: QueuePresentationState;
    window: QueueWindow;
    selectionState: ReturnType<typeof deriveSelectionState>;
    canStartDownloads: boolean;
    viewport: HTMLElement | null;
    selectAll: HTMLInputElement | null;
    titleEditorInput: HTMLInputElement | null;
    onScroll: (event: Event) => void;
    onSelectionChange: (event: Event) => void;
    setSelected: (itemId: string, selected: boolean) => void;
    setDraft: (draft: string) => void;
    beginFilenameEdit: (item: QueueItem) => void | Promise<void>;
    commitFilenameEdit: (itemId: string) => void | Promise<void>;
    handleFilenameEditorKeydown: (event: KeyboardEvent) => void;
    toggleDiagnostics: (itemId: string) => void;
    changeQuality: (item: QueueItem, event: Event) => void | Promise<void>;
    changeFormat: (item: QueueItem, event: Event) => void | Promise<void>;
    downloadItem: (item: QueueItem) => void | Promise<void>;
    cancelItem: (item: QueueItem) => void | Promise<void>;
    retryItem: (item: QueueItem) => void | Promise<void>;
    copyDiagnostics: (item: QueueItem) => void | Promise<void>;
  } = $props();
</script>

<section
  class="queue"
  bind:this={viewport}
  onscroll={onScroll}
  data-queue-count={state.items.length}
>
  {#if state.items.length === 0}
    <div class="empty-state">
      <p>No videos in queue. Paste a video URL above to get started.</p>
    </div>
  {:else}
    <table aria-rowcount={state.items.length + 1}>
      <thead>
        <tr>
          <th class="col-check">
            <input
              bind:this={selectAll}
              type="checkbox"
              checked={selectionState === 'all'}
              onchange={onSelectionChange}
              aria-label="Select all queue items"
            />
          </th>
          <th class="col-title">Title</th>
          <th class="col-status">Status</th>
          <th class="col-quality">Quality</th>
          <th class="col-format">Format</th>
          <th class="col-progress">Progress</th>
          <th class="col-speed">Speed</th>
          <th class="col-eta">ETA</th>
          <th class="col-actions"></th>
        </tr>
      </thead>
      <tbody>
        {#if window.topSpacerHeight > 0}
          <tr class="virtual-spacer" aria-hidden="true">
            <td colspan="9" style={`height: ${window.topSpacerHeight}px`}></td>
          </tr>
        {/if}
        {#each window.rows as row (row.item.id)}
          <QueueRow
            item={row.item}
            index={row.index}
            selected={state.items[row.index].selected}
            editing={state.editing.itemId === row.item.id}
            draft={state.editing.draft}
            editingError={state.editing.error}
            bind:titleEditorInput
            {canStartDownloads}
            {setSelected}
            {setDraft}
            {beginFilenameEdit}
            {commitFilenameEdit}
            {handleFilenameEditorKeydown}
            {toggleDiagnostics}
            {changeQuality}
            {changeFormat}
            {downloadItem}
            {cancelItem}
            {retryItem}
            {copyDiagnostics}
          />
        {/each}
        {#if window.bottomSpacerHeight > 0}
          <tr class="virtual-spacer" aria-hidden="true">
            <td colspan="9" style={`height: ${window.bottomSpacerHeight}px`}></td>
          </tr>
        {/if}
      </tbody>
    </table>
  {/if}
</section>
