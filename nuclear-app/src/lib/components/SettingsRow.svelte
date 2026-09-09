<script lang="ts">
  import {
    audioFormats,
    supportedBrowsers,
    videoFormats,
    type BrowserName,
    type CookieMode,
    type OutputFormat
  } from '$lib/frontend-types';
  import { getPathBasename } from '$lib/settings-diagnostics-workflow';

  let {
    globalQuality = $bindable(),
    globalFormat = $bindable(),
    outputDir = $bindable(),
    outputDirError,
    useCookies = $bindable(),
    cookieMode = $bindable(),
    cookieBrowser = $bindable(),
    cookieFilePath,
    compatConfigPath = $bindable(),
    applyGlobalQuality,
    applyGlobalFormat,
    browseOutputDir,
    browseCookieFile,
    browseCompatConfigFile
  }: {
    globalQuality: string;
    globalFormat: OutputFormat;
    outputDir: string;
    outputDirError: string | null;
    useCookies: boolean;
    cookieMode: CookieMode;
    cookieBrowser: BrowserName;
    cookieFilePath: string;
    compatConfigPath: string;
    applyGlobalQuality: () => void | Promise<void>;
    applyGlobalFormat: () => void | Promise<void>;
    browseOutputDir: () => void | Promise<void>;
    browseCookieFile: () => void | Promise<void>;
    browseCompatConfigFile: () => void | Promise<void>;
  } = $props();
</script>

<section class="settings-row">
  <div class="setting">
    <label for="quality">Quality</label>
    <select id="quality" bind:value={globalQuality} onchange={applyGlobalQuality}>
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
    <label for="format">Format</label>
    <select id="format" bind:value={globalFormat} onchange={applyGlobalFormat}>
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
    <label for="outdir">Output</label>
    <input id="outdir" type="text" bind:value={outputDir} readonly />
    <button onclick={browseOutputDir}>Browse</button>
    {#if outputDirError}
      <span class="error-text" role="alert">{outputDirError}</span>
    {/if}
  </div>
  <div class="setting cookie-setting">
    <label>
      <input type="checkbox" bind:checked={useCookies} />
      Cookies
    </label>
    {#if useCookies}
      <label class="sr-only" for="cookie-mode">Cookie source</label>
      <select id="cookie-mode" bind:value={cookieMode} class="cookie-mode-select">
        <option value="browser">From Browser</option>
        <option value="file">From File</option>
      </select>
      {#if cookieMode === 'browser'}
        <label class="sr-only" for="cookie-browser">Cookie browser</label>
        <select id="cookie-browser" bind:value={cookieBrowser}>
          {#each supportedBrowsers as b (b)}
            <option value={b}>{b.charAt(0).toUpperCase() + b.slice(1)}</option>
          {/each}
        </select>
        {#if cookieBrowser === 'chrome' || cookieBrowser === 'edge' || cookieBrowser === 'brave' || cookieBrowser === 'chromium'}
          <span class="cookie-warn"
            >Chromium browsers block cookie access — use Firefox or a cookie file instead</span
          >
        {:else}
          <span class="cookie-hint">Close {cookieBrowser} first if errors occur</span>
        {/if}
      {:else}
        <button class="cookie-browse" onclick={browseCookieFile}>
          {cookieFilePath ? cookieFilePath.split(/[\\/]/).pop() : 'Select cookies.txt'}
        </button>
        <span class="cookie-hint"
          >Export via browser extension (e.g. "Get cookies.txt LOCALLY")</span
        >
      {/if}
    {/if}
  </div>
  <div class="setting advanced-config">
    <label for="compat-config">Compat Config</label>
    <button id="compat-config" class="cookie-browse" onclick={browseCompatConfigFile}>
      {compatConfigPath ? getPathBasename(compatConfigPath) : 'None'}
    </button>
    {#if compatConfigPath}
      <button class="small" onclick={() => (compatConfigPath = '')}>Clear</button>
    {/if}
  </div>
</section>
