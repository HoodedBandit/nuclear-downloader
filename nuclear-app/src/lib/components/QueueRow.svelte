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
  import Icon from './Icon.svelte';
  import { sourceLabel } from '$lib/queue-view';

  let failedThumbnail = $state<string | null>(null);
  import { tick } from 'svelte';
  import type { QueueRowActions, FilenameEditorActions } from '$lib/queue-row-actions';
  import { focusFilenameInput } from '$lib/filename-editor-focus';
  let {
    item,
    index,
    selected,
    canStartDownloads,
    actions,
    editor
  }: {
    item: QueueItem;
    index: number;
    selected: boolean;
    canStartDownloads: boolean;
    actions: QueueRowActions;
    editor: FilenameEditorActions;
  } = $props();
  let titleButton = $state<HTMLButtonElement | null>(null);
  const editing = $derived(editor.state.itemId === item.id);
  async function editorKeydown(event: KeyboardEvent): Promise<void> {
    if (event.isComposing) return;
    if (event.key === 'Escape') {
      event.preventDefault();
      editor.cancel();
    } else if (event.key === 'Enter') {
      event.preventDefault();
      if (!(await editor.commit(item.id))) return;
    } else return;
    await tick();
    titleButton?.focus();
  }
</script>

<tr
  class="queue-item"
  class:downloading={item.status === 'downloading'}
  class:selected
  aria-rowindex={index + 2}
>
  <td class="col-check"
    ><input
      type="checkbox"
      checked={selected}
      onchange={(event) => actions.select(item.id, event.currentTarget.checked)}
      aria-label={`Select ${getQueueItemDisplayTitle(item)}`}
    /></td
  >
  <td class="col-title" title={item.url}
    ><div class="title-cell">
      {#if item.thumbnail && item.thumbnail !== failedThumbnail}<img
          src={item.thumbnail}
          alt=""
          class="thumb"
          loading="lazy"
          decoding="async"
          referrerpolicy="no-referrer"
          onerror={() => {
            failedThumbnail = item.thumbnail;
          }}
        />{:else}<span class="thumb thumb-placeholder"
          ><Icon name={isAudioOnlyFormat(item.format) ? 'music' : 'video'} size={23} /></span
        >{/if}
      <div class="title-info">
        {#if editing}
          <input
            use:focusFilenameInput
            value={editor.state.draft}
            oninput={(event) => editor.setDraft(event.currentTarget.value)}
            type="text"
            class="title-editor"
            aria-label="Edit queued filename"
            onblur={() => editor.commit(item.id)}
            onkeydown={editorKeydown}
            onclick={(event) => event.stopPropagation()}
          />
          {#if editor.state.error}<span class="filename-error" role="alert"
              >Check Settings for filename details.</span
            >{/if}
        {:else if canEditFilename(item)}
          <button
            type="button"
            class="title-button"
            bind:this={titleButton}
            title="Click to edit the filename before download"
            onclick={() => editor.begin(item)}
            ><span class="title-text">{getQueueItemDisplayTitle(item)}</span></button
          >
        {:else}<span class="title-text">{getQueueItemDisplayTitle(item)}</span>{/if}
        <div class="media-meta">
          <span class="channel">{item.channel || sourceLabel(item.url)}</span
          >{#if item.duration}<span aria-hidden="true">·</span><span class="duration"
              >{formatDuration(item.duration)}</span
            >{/if}
        </div>
      </div>
    </div></td
  >
  <td class="col-options">
    <div class="item-options">
      <span class="col-format"
        >{#if isEditablePendingStatus(item.status) && item.infoLoaded}<select
            value={item.format}
            onchange={(event) => actions.format(item, event.currentTarget.value)}
            aria-label={`Format for ${getQueueItemDisplayTitle(item)}`}
          >
            <optgroup label="Video"
              >{#each videoFormats as fmt (fmt)}<option value={fmt}>{fmt.toUpperCase()}</option
                >{/each}</optgroup
            >
            <optgroup label="Audio"
              >{#each audioFormats as fmt (fmt)}<option
                  value={fmt}
                  disabled={item.hasAudio === false && isAudioOnlyFormat(fmt)}
                  >{fmt.toUpperCase()}</option
                >{/each}</optgroup
            >
          </select>{:else}{item.format.toUpperCase()}{/if}</span
      >
      <span class="option-dot" aria-hidden="true">·</span>
      <span class="col-quality"
        >{#if isEditablePendingStatus(item.status) && item.infoLoaded}<select
            value={item.quality}
            onchange={(event) => actions.quality(item, event.currentTarget.value)}
            aria-label={`Quality for ${getQueueItemDisplayTitle(item)}`}
            >{#each item.availableQualities as q (q)}<option value={q}
                >{q === 'best' ? 'Best' : q === '2160p' ? '4K' : q}</option
              >{/each}</select
          >{:else}{item.quality === 'best'
            ? 'Best'
            : item.quality === '2160p'
              ? '4K'
              : item.quality}{/if}</span
      >
    </div>
  </td>
  <td class="col-progress"
    ><div class="row-progress">
      <div class="col-status" class:error-status={item.status === 'error'}>
        {#if item.status === 'completed'}<span class="completed-icon"
            ><Icon name="check" size={14} /></span
          >
        {:else if item.status === 'fetching' || item.status === 'postprocessing' || item.status === 'cancelling'}<span
            class="spinner"
            aria-hidden="true"
          ></span>
        {:else if item.status === 'ready' || item.status === 'queued'}<Icon
            name="clock"
            size={19}
          />
        {:else if item.status === 'error'}<Icon name="alert" size={18} />{/if}
        <span class="status-pill {item.status}">{getStatusLabel(item)}</span>
        {#if item.status === 'downloading'}<span class="progress-text"
            >{roundedProgress(item.progress)}%</span
          >{/if}
        {#if item.error}<button
            type="button"
            class="error-tooltip text-button"
            title="View error in Settings"
            aria-label={`Details for ${getQueueItemDisplayTitle(item)}`}
            onclick={() => actions.details(item.id)}>Details</button
          >{/if}
      </div>
      {#if item.status === 'downloading'}
        <div
          class="progress-bar"
          role="progressbar"
          aria-label={`Download progress for ${getQueueItemDisplayTitle(item)}`}
          aria-valuenow={roundedProgress(item.progress)}
          aria-valuemin="0"
          aria-valuemax="100"
        >
          <div class="progress-fill" style:width={`${roundedProgress(item.progress)}%`}></div>
        </div>
        <div class="transfer-meta">
          <span class="col-speed">{item.speed}</span>{#if item.speed && item.eta}<span
              aria-hidden="true">·</span
            >{/if}<span class="col-eta">{item.eta ? `${item.eta} left` : ''}</span>
        </div>
      {:else if shouldShowConversionProgress(item) && item.status === 'postprocessing'}
        <div class="phase-progress">
          <span class="phase-label">Download complete</span>
          <div
            class="progress-bar"
            role="progressbar"
            aria-label="Conversion progress"
            aria-valuemin="0"
            aria-valuemax="100"
            aria-valuenow={item.conversionProgress === null
              ? undefined
              : roundedProgress(item.conversionProgress)}
          >
            <div
              class="progress-fill convert"
              class:indeterminate={item.conversionProgress === null}
              style:width={`${item.conversionProgress === null ? 30 : roundedProgress(item.conversionProgress)}%`}
            ></div>
          </div>
          <span class="progress-text"
            >{item.conversionProgress === null
              ? 'Preparing conversion'
              : `${roundedProgress(item.conversionProgress)}%`}</span
          >
        </div>
      {/if}
      {#if item.error}<span class="error-summary" role="status">Check Settings for details.</span
        >{/if}
    </div></td
  >
  <td class="col-actions">
    {#if isEditablePendingStatus(item.status)}<button
        class="icon-button row-action"
        onclick={() => actions.download(item)}
        disabled={!canStartDownloads}
        aria-label={`Download ${getQueueItemDisplayTitle(item)}`}
        title="Download"><Icon name="download" /></button
      >
    {:else if item.status === 'fetching' || item.status === 'downloading' || item.status === 'postprocessing' || item.status === 'cancelling'}<button
        class="icon-button row-action"
        onclick={() => actions.cancel(item)}
        disabled={item.status === 'cancelling'}
        aria-label={`Cancel ${getQueueItemDisplayTitle(item)}`}
        title="Cancel download"><Icon name="stop" size={19} /></button
      >
    {:else if canRetryItem(item)}<button
        class="row-action retry-action"
        onclick={() => actions.retry(item)}
        ><Icon name="retry" size={17} /><span>Retry</span></button
      >
    {:else if item.status === 'completed' && item.filename}<button
        class="row-action reveal-action"
        onclick={() => actions.reveal(item)}
        title="Show in folder"
        aria-label={`Show ${getQueueItemDisplayTitle(item)} in folder`}
        ><Icon name="folder" size={17} /><span>Show in folder</span></button
      >{/if}
  </td>
</tr>
