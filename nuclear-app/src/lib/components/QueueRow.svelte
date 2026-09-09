<script lang="ts">
  import type { QueueItem } from '$lib/frontend-types';
  import { audioFormats, videoFormats } from '$lib/frontend-types';
  import { isAudioOnlyFormat } from '$lib/queue-logic';
  import {
    canEditFilename,
    canRetryItem,
    formatDuration,
    getQueueItemDisplayTitle,
    getStatusLabel,
    isEditablePendingStatus,
    roundedProgress,
    shouldShowConversionProgress
  } from '$lib/queue-presentation';
  import RowDiagnostics from './RowDiagnostics.svelte';

  let {
    item,
    index,
    selected,
    editing,
    draft,
    editingError,
    titleEditorInput = $bindable(),
    canStartDownloads,
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
    item: QueueItem;
    index: number;
    selected: boolean;
    editing: boolean;
    draft: string;
    editingError: string;
    titleEditorInput: HTMLInputElement | null;
    canStartDownloads: boolean;
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

<tr class="queue-item" class:downloading={item.status === 'downloading'} aria-rowindex={index + 2}>
  <td class="col-check">
    <input
      type="checkbox"
      checked={selected}
      onchange={(event) => setSelected(item.id, event.currentTarget.checked)}
      aria-label={`Select ${getQueueItemDisplayTitle(item)}`}
    />
  </td>
  <td class="col-title" title={item.url}>
    <div class="title-cell">
      {#if item.thumbnail}
        <img
          src={item.thumbnail}
          alt=""
          class="thumb"
          loading="lazy"
          decoding="async"
          referrerpolicy="no-referrer"
        />
      {/if}
      <div class="title-info">
        {#if editing}
          <input
            bind:this={titleEditorInput}
            value={draft}
            oninput={(event) => setDraft(event.currentTarget.value)}
            type="text"
            class="title-editor"
            aria-label="Edit queued filename"
            onblur={() => commitFilenameEdit(item.id)}
            onkeydown={handleFilenameEditorKeydown}
            onclick={(event) => event.stopPropagation()}
          />
          {#if editingError}
            <span class="filename-error" role="alert">{editingError}</span>
          {/if}
        {:else if canEditFilename(item)}
          <button
            type="button"
            class="title-button"
            title="Click to edit the filename before download"
            onclick={() => beginFilenameEdit(item)}
          >
            <span class="title-text">{getQueueItemDisplayTitle(item)}</span>
          </button>
        {:else}
          <span class="title-text">{getQueueItemDisplayTitle(item)}</span>
        {/if}
        {#if item.channel}<span class="channel">{item.channel}</span>{/if}
        {#if item.duration}<span class="duration">{formatDuration(item.duration)}</span>{/if}
      </div>
    </div>
  </td>
  <td class="col-status">
    <span class="status-pill {item.status}">{getStatusLabel(item)}</span>
    {#if item.error}
      <button
        type="button"
        class="error-tooltip"
        title="Show diagnostics"
        onclick={() => toggleDiagnostics(item.id)}>!</button
      >
      <span class="error-summary" title={item.error} role="alert">{item.error}</span>
    {/if}
  </td>
  <td class="col-quality">
    {#if isEditablePendingStatus(item.status)}
      <select
        value={item.quality}
        onchange={(event) => changeQuality(item, event)}
        aria-label={`Quality for ${getQueueItemDisplayTitle(item)}`}
      >
        {#each item.availableQualities as q (q)}
          <option value={q}>{q === 'best' ? 'Best' : q}</option>
        {/each}
      </select>
    {:else}<span class="muted">{item.quality}</span>{/if}
  </td>
  <td class="col-format">
    {#if isEditablePendingStatus(item.status)}
      <select
        value={item.format}
        onchange={(event) => changeFormat(item, event)}
        aria-label={`Format for ${getQueueItemDisplayTitle(item)}`}
      >
        <optgroup label="Video">
          {#each videoFormats as fmt (fmt)}<option value={fmt}>{fmt.toUpperCase()}</option>{/each}
        </optgroup>
        <optgroup label="Audio">
          {#each audioFormats as fmt (fmt)}
            <option value={fmt} disabled={item.hasAudio === false && isAudioOnlyFormat(fmt)}>
              {fmt.toUpperCase()}
            </option>
          {/each}
        </optgroup>
      </select>
    {:else}<span class="muted">{item.format.toUpperCase()}</span>{/if}
  </td>
  <td class="col-progress">
    {#if shouldShowConversionProgress(item)}
      <div class="phase-progress">
        <div class="phase-progress-row">
          <span class="phase-label">DL</span>
          <div class="progress-bar">
            <div
              class="progress-fill"
              class:complete={item.downloadProgress >= 100}
              class:error={item.status === 'error' && item.conversionProgress === null}
              style="width: {item.downloadProgress}%"
            ></div>
            <span class="progress-text">{roundedProgress(item.downloadProgress)}%</span>
          </div>
        </div>
        <div class="phase-progress-row">
          <span class="phase-label">CV</span>
          <div class="progress-bar">
            <div
              class="progress-fill convert"
              class:complete={item.status === 'completed'}
              class:error={item.status === 'error' && item.conversionProgress !== null}
              style="width: {item.conversionProgress ?? 0}%"
            ></div>
            <span class="progress-text">{roundedProgress(item.conversionProgress)}%</span>
          </div>
        </div>
      </div>
    {:else}
      <div class="progress-bar">
        <div
          class="progress-fill"
          class:complete={item.status === 'completed'}
          class:error={item.status === 'error'}
          style="width: {item.progress}%"
        ></div>
        <span class="progress-text">{roundedProgress(item.progress)}%</span>
      </div>
    {/if}
  </td>
  <td class="col-speed"><span class="muted">{item.speed}</span></td>
  <td class="col-eta"><span class="muted">{item.eta}</span></td>
  <td class="col-actions">
    {#if isEditablePendingStatus(item.status)}
      <button
        class="small primary"
        onclick={() => downloadItem(item)}
        disabled={!canStartDownloads}
        aria-label={`Download ${getQueueItemDisplayTitle(item)}`}>DL</button
      >
    {:else if item.status === 'downloading' || item.status === 'postprocessing' || item.status === 'cancelling'}
      <button
        class="small danger"
        onclick={() => cancelItem(item)}
        disabled={item.status === 'cancelling'}
        aria-label={`Cancel ${getQueueItemDisplayTitle(item)}`}
        >{item.status === 'cancelling' ? '...' : 'X'}</button
      >
    {:else if canRetryItem(item)}
      <button class="small" onclick={() => retryItem(item)}>Retry</button>
    {/if}
  </td>
</tr>
{#if item.diagnosticsOpen && item.error}
  <RowDiagnostics {item} {copyDiagnostics} />
{/if}
