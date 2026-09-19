<script lang="ts">
  import Icon from './Icon.svelte';
  let {
    summary,
    canStartDownloads,
    downloadAll,
    downloadSelected,
    removeSelected,
    clearCompleted,
    cancelAll,
    selectAll,
    selectionState,
    selectedCount
  }: {
    summary: {
      hasReady: boolean;
      hasSelectedReady: boolean;
      hasSelected: boolean;
      hasCompleted: boolean;
      hasActive: boolean;
    };
    canStartDownloads: boolean;
    downloadAll: () => void;
    downloadSelected: () => void;
    removeSelected: () => void;
    clearCompleted: () => void;
    cancelAll: () => void;
    selectAll: () => void;
    selectionState: 'none' | 'some' | 'all';
    selectedCount: number;
  } = $props();
</script>

<section class="actions" aria-label="Queue actions">
  <div class="queue-toolbar">
    <div class="queue-heading">
      <h2>Queue</h2>
      <button
        class="text-button"
        aria-pressed={selectionState === 'some' ? 'mixed' : selectionState === 'all'}
        onclick={selectAll}>{selectionState === 'all' ? 'Deselect all' : 'Select all'}</button
      >{#if selectedCount}<span class="selected-count">{selectedCount} selected</span>{/if}
    </div>
    <div class="queue-toolbar-actions">
      {#if summary.hasSelected}<button
          class="icon-button"
          title="Remove selected"
          aria-label="Remove selected"
          onclick={removeSelected}><Icon name="trash" size={18} /></button
        >{/if}
      <details class="queue-menu">
        <summary aria-label="More queue actions" title="More queue actions"
          ><Icon name="more" /></summary
        >
        <div class="menu-popover">
          <button onclick={clearCompleted} disabled={!summary.hasCompleted}
            ><Icon name="check" size={17} />Clear completed</button
          ><button class="danger-text" onclick={cancelAll} disabled={!summary.hasActive}
            ><Icon name="stop" size={17} />Cancel all downloads</button
          >
        </div>
      </details>
      <button
        class="primary"
        onclick={summary.hasSelected ? downloadSelected : downloadAll}
        disabled={!canStartDownloads ||
          (summary.hasSelected ? !summary.hasSelectedReady : !summary.hasReady)}
        ><Icon name="download" size={19} />{summary.hasSelected
          ? 'Download selected'
          : 'Download queued'}</button
      >
    </div>
  </div>
</section>
