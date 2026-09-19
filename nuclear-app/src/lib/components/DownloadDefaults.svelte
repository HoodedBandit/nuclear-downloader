<script lang="ts">
  import Icon from './Icon.svelte';
  import { audioFormats, videoFormats } from '$lib/frontend-types';
  import {
    getPathBasename,
    type SettingsDiagnosticsState
  } from '$lib/settings-diagnostics-workflow';
  let {
    state,
    onQuality,
    onFormat,
    browseOutputDir
  }: {
    state: SettingsDiagnosticsState;
    onQuality: () => void | Promise<void>;
    onFormat: () => void | Promise<void>;
    browseOutputDir: () => void | Promise<void>;
  } = $props();
</script>

<section class="settings-row">
  <div class="setting">
    <label for="quality">Default quality</label>
    <select id="quality" bind:value={state.globalQuality} onchange={onQuality}>
      <option value="best">Best</option>
      <option value="2160p">4K</option>
      <option value="1440p">1440p</option>
      <option value="1080p">1080p</option>
      <option value="720p">720p</option>
      <option value="480p">480p</option>
      <option value="360p">360p</option>
    </select>
  </div>
  <div class="setting">
    <label for="format">Default format</label>
    <select id="format" bind:value={state.globalFormat} onchange={onFormat}>
      <optgroup label="Video">
        {#each videoFormats as fmt (fmt)}
          <option value={fmt}>{fmt.toUpperCase()}</option>
        {/each}
      </optgroup>
      <optgroup label="Audio Only">
        {#each audioFormats as fmt (fmt)}
          <option value={fmt}>{fmt.toUpperCase()}</option>
        {/each}
      </optgroup>
    </select>
  </div>
  <div class="setting output-dir">
    <label for="outdir">Save to</label>
    <button
      id="outdir"
      class="folder-picker"
      onclick={browseOutputDir}
      title={state.outputDir}
      aria-label="Choose output folder"
      ><Icon name="folder" size={20} /><span
        >{getPathBasename(state.outputDir) || 'Choose folder'}</span
      ><Icon name="chevron" size={15} /></button
    >
    {#if state.outputDirError}
      <span class="error-text" role="alert">Check Settings for folder details.</span>
    {/if}
  </div>
</section>
