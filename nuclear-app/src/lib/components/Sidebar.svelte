<script lang="ts">
  import Icon from './Icon.svelte';
  import type { IconName } from './Icon.svelte';
  import type { QueueFilter } from '$lib/queue-view';
  let {
    filter,
    counts,
    onFilter,
    onSettings,
    onHelp,
    unreadErrors = 0
  }: {
    unreadErrors?: number;
    filter: QueueFilter;
    counts: Record<QueueFilter, number>;
    onFilter: (filter: QueueFilter) => void;
    onSettings: () => void;
    onHelp: () => void;
  } = $props();
  const entries: { id: QueueFilter; label: string; icon: IconName }[] = [
    { id: 'all', label: 'All downloads', icon: 'grid' },
    { id: 'active', label: 'In progress', icon: 'activity' },
    { id: 'queued', label: 'Queued', icon: 'clock' },
    { id: 'completed', label: 'Completed', icon: 'check' }
  ];
</script>

<aside class="sidebar" aria-label="Sidebar">
  <div class="app-brand">
    <img class="brand-mark" src="/app-icon.png" alt="" width="32" height="32" /><span
      >Nuclear Downloader</span
    >
  </div>
  <nav aria-label="Downloads">
    <p class="nav-caption">Downloads</p>
    {#each entries as entry (entry.id)}<button
        class="nav-item"
        class:active={filter === entry.id}
        aria-current={filter === entry.id ? 'page' : undefined}
        onclick={() => onFilter(entry.id)}
        ><Icon name={entry.icon} /><span>{entry.label}</span><span class="nav-count"
          >{counts[entry.id]}</span
        ></button
      >{/each}
    {#if counts.attention}<button
        class="nav-item"
        class:active={filter === 'attention'}
        aria-current={filter === 'attention' ? 'page' : undefined}
        onclick={() => onFilter('attention')}
        ><Icon name="alert" /><span>Needs attention</span><span class="nav-count"
          >{counts.attention}</span
        ></button
      >{/if}
  </nav>
  <div class="sidebar-bottom">
    <button
      class="nav-item settings-nav"
      aria-label={unreadErrors ? `Settings, ${unreadErrors} unread errors` : 'Settings'}
      onclick={onSettings}
      ><span class="settings-icon" class:has-unread={unreadErrors > 0}
        ><Icon name="settings" />{#if unreadErrors}<span class="notification-dot" aria-hidden="true"
          ></span>{/if}</span
      ><span>Settings</span></button
    ><button class="nav-item" onclick={onHelp}><Icon name="help" /><span>Help</span></button>
  </div>
</aside>
