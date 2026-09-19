<script lang="ts">
  import type { ErrorEntry } from '$lib/error-inbox';
  import type { Snippet } from 'svelte';
  import Icon from './Icon.svelte';
  import { accessibleDialog } from '$lib/accessible-dialog';
  import type { ThemePreference } from '$lib/theme';
  let {
    errors,
    theme,
    saving,
    error,
    onTheme,
    onClose,
    advanced,
    runtime,
    diagnostics
  }: {
    errors: ErrorEntry[];
    theme: ThemePreference;
    saving: boolean;
    error: string | null;
    onTheme: (theme: ThemePreference) => void;
    onClose: () => void;
    advanced: Snippet;
    runtime: Snippet;
    diagnostics: Snippet;
  } = $props();
</script>

<div class="modal-layer">
  <button class="modal-backdrop" aria-label="Close settings" onclick={onClose}></button>
  <div
    class="modal settings-dialog"
    role="dialog"
    aria-modal="true"
    aria-labelledby="settings-title"
    tabindex="-1"
    use:accessibleDialog={{ onClose }}
  >
    <div class="modal-header">
      <div>
        <h2 id="settings-title">Settings</h2>
        <p class="modal-subtitle">Make Nuclear Downloader feel at home.</p>
      </div>
      <button
        class="icon-button"
        aria-label="Close settings"
        data-dialog-initial-focus
        onclick={onClose}><Icon name="close" /></button
      >
    </div>
    <div class="settings-body">
      <section class="settings-section errors-section" aria-labelledby="errors-title">
        <div class="section-heading">
          <h3 id="errors-title">Errors</h3>
          <span class="error-count">{errors.length}</span>
        </div>
        {#if errors.length}
          <p class="section-description">
            Recent errors from this session. Opening Settings marks them as seen.
          </p>
          <div class="error-history">
            {#each errors as entry (entry.id)}
              <details class="error-entry">
                <summary
                  ><Icon name="alert" size={18} /><span>{entry.context}</span><time
                    datetime={new Date(entry.occurredAt).toISOString()}
                    >{new Date(entry.occurredAt).toLocaleTimeString([], {
                      hour: '2-digit',
                      minute: '2-digit'
                    })}</time
                  ><Icon name="chevron" size={16} /></summary
                >
                <pre>{entry.detail}</pre>
              </details>
            {/each}
          </div>
        {:else}<p class="errors-empty">
            <Icon name="check" size={18} />You're all caught up. No errors this session.
          </p>{/if}
      </section>
      <section class="settings-section">
        <h3>Appearance</h3>
        <p class="section-description">Choose a look, or follow your device.</p>
        <div class="theme-options" role="group" aria-label="Color theme">
          {#each ['light', 'dark', 'system'] as value (value)}
            {@const option = value as ThemePreference}
            <button
              class="theme-option"
              class:selected={theme === option}
              aria-pressed={theme === option}
              disabled={saving}
              onclick={() => onTheme(option)}
            >
              <span class="theme-swatch {option}"
                ><span class="swatch-sidebar"></span><span class="swatch-content"
                  ><i></i><i></i><i></i></span
                ></span
              >
              <span class="theme-label"
                ><Icon
                  name={option === 'light' ? 'sun' : option === 'dark' ? 'moon' : 'system'}
                  size={17}
                />{option === 'light'
                  ? 'Light'
                  : option === 'dark'
                    ? 'Dark'
                    : 'System'}{#if theme === option}<Icon name="check" size={16} />{/if}</span
              >
            </button>
          {/each}
        </div>
        {#if error}<p class="error-text" role="alert">{error}</p>{/if}
      </section>
      <section class="settings-section">
        <h3>Download access</h3>
        <p class="section-description">Optional authentication and compatibility settings.</p>
        {@render advanced()}
      </section>
      <section class="settings-section">
        <h3>Application &amp; download tools</h3>
        <p class="section-description">Version information, runtime health, and updates.</p>
        {@render runtime()}
      </section>
      <section class="settings-section">
        <h3>Diagnostics</h3>
        <p class="section-description">Export a redacted report to help investigate an issue.</p>
        {@render diagnostics()}
      </section>
    </div>
  </div>
</div>
