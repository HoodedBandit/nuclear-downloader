<script lang="ts">
  import { supportedBrowsers } from '$lib/frontend-types';
  import {
    getPathBasename,
    type SettingsDiagnosticsState
  } from '$lib/settings-diagnostics-workflow';
  let {
    state,
    browseCookies,
    browseConfig
  }: {
    state: SettingsDiagnosticsState;
    browseCookies: () => void | Promise<void>;
    browseConfig: () => void | Promise<void>;
  } = $props();
</script>

<section class="advanced-settings">
  <div class="setting cookie-setting">
    <label>
      <input type="checkbox" bind:checked={state.useCookies} />
      Use browser cookies
    </label>
    {#if state.useCookies}
      <label class="sr-only" for="cookie-mode">Cookie source</label>
      <select id="cookie-mode" bind:value={state.cookieMode} class="cookie-mode-select">
        <option value="browser">From Browser</option>
        <option value="file">From File</option>
      </select>
      {#if state.cookieMode === 'browser'}
        <label class="sr-only" for="cookie-browser">Cookie browser</label>
        <select id="cookie-browser" bind:value={state.cookieBrowser}>
          {#each supportedBrowsers as b (b)}
            <option value={b}>{b.charAt(0).toUpperCase() + b.slice(1)}</option>
          {/each}
        </select>
        {#if state.cookieBrowser === 'chrome' || state.cookieBrowser === 'edge' || state.cookieBrowser === 'brave' || state.cookieBrowser === 'chromium'}
          <span class="cookie-warn"
            >Chromium browsers block cookie access — use Firefox or a cookie file instead</span
          >
        {:else}
          <span class="cookie-hint">Close {state.cookieBrowser} first if errors occur</span>
        {/if}
      {:else}
        <button class="cookie-browse" onclick={browseCookies}>
          {state.cookieFilePath ? state.cookieFilePath.split(/[\\/]/).pop() : 'Select cookies.txt'}
        </button>
        <span class="cookie-hint"
          >Export via browser extension (e.g. "Get cookies.txt LOCALLY")</span
        >
      {/if}
    {/if}
  </div>
  <div class="setting advanced-config">
    <label for="compat-config">Compat Config</label>
    <button id="compat-config" class="cookie-browse" onclick={browseConfig}>
      {state.compatConfigPath ? getPathBasename(state.compatConfigPath) : 'None'}
    </button>
    {#if state.compatConfigPath}
      <button class="small" onclick={() => (state.compatConfigPath = '')}>Clear</button>
    {/if}
  </div>
  {#if state.accessError}<p class="error-text" role="alert">{state.accessError}</p>{/if}
</section>
