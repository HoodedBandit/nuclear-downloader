<script lang="ts">
  import { accessibleDialog } from '$lib/accessible-dialog';
  import type { PlaylistModal, PlaylistModalEntry } from '$lib/frontend-types';
  import type { SelectionState } from '$lib/queue-logic';

  interface VisiblePlaylistEntry {
    entry: PlaylistModalEntry;
    index: number;
  }

  interface Props {
    modal: PlaylistModal;
    page: number;
    pageCount: number;
    visibleEntries: VisiblePlaylistEntry[];
    selectionState: SelectionState;
    formatDuration: (seconds: number | null | undefined) => string;
    onClose: () => void;
    onToggleAll: (checked: boolean) => void;
    onToggleEntry: (index: number, checked: boolean) => void;
    onChangePage: (delta: number) => void;
    onAddSelection: () => void | Promise<void>;
  }

  let {
    modal,
    page,
    pageCount,
    visibleEntries,
    selectionState,
    formatDuration,
    onClose,
    onToggleAll,
    onToggleEntry,
    onChangePage,
    onAddSelection
  }: Props = $props();

  let selectAll = $state<HTMLInputElement | null>(null);
  let selectedCount = $derived(modal.entries.filter((entry) => entry.selected).length);

  $effect(() => {
    if (selectAll) selectAll.indeterminate = selectionState === 'some';
  });
</script>

<div class="modal-layer">
  <button type="button" class="modal-backdrop" aria-label="Close playlist picker" onclick={onClose}
  ></button>
  <div
    class="modal"
    role="dialog"
    aria-modal="true"
    aria-labelledby="playlist-modal-title"
    tabindex="-1"
    use:accessibleDialog={{ onClose }}
  >
    <div class="modal-header">
      <div>
        <h2 id="playlist-modal-title">{modal.info.title}</h2>
        {#if modal.info.channel}
          <span class="modal-channel">{modal.info.channel}</span>
        {/if}
        <span class="modal-count">
          {modal.info.truncated
            ? `Showing first ${modal.info.entry_count} videos`
            : `${modal.info.entry_count} videos`}
        </span>
      </div>
      <button class="small" onclick={onClose} data-dialog-initial-focus
        >Close playlist picker</button
      >
    </div>
    <div class="modal-controls">
      <label class="select-all-label">
        <input
          bind:this={selectAll}
          type="checkbox"
          checked={selectionState === 'all'}
          onchange={(event) => onToggleAll(event.currentTarget.checked)}
        />
        Select All
      </label>
      <span class="muted"> {selectedCount} of {modal.entries.length} selected </span>
    </div>
    <div class="modal-list">
      {#each visibleEntries as row (row.entry.url)}
        {@const entry = row.entry}
        <label class="playlist-entry" class:entry-selected={entry.selected}>
          <input
            type="checkbox"
            checked={entry.selected}
            onchange={(event) => onToggleEntry(row.index, event.currentTarget.checked)}
          />
          {#if entry.thumbnail}
            <img
              src={entry.thumbnail}
              alt=""
              class="entry-thumb"
              loading="lazy"
              decoding="async"
              referrerpolicy="no-referrer"
            />
          {/if}
          <div class="entry-info">
            <span class="entry-title">{entry.title || entry.id}</span>
            {#if entry.duration}
              <span class="entry-duration">{formatDuration(entry.duration)}</span>
            {/if}
          </div>
        </label>
      {/each}
    </div>
    {#if pageCount > 1}
      <div class="modal-pagination">
        <button class="small" onclick={() => onChangePage(-1)} disabled={page === 0}
          >Previous</button
        >
        <span class="muted">Page {page + 1} of {pageCount}</span>
        <button class="small" onclick={() => onChangePage(1)} disabled={page >= pageCount - 1}
          >Next</button
        >
      </div>
    {/if}
    <div class="modal-footer">
      <button class="primary" onclick={onAddSelection} disabled={selectedCount === 0}>
        Add {selectedCount} Videos to Queue
      </button>
      <button onclick={onClose}>Cancel</button>
    </div>
  </div>
</div>
