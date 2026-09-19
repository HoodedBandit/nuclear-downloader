<script lang="ts">
  export type IconName =
    | 'download'
    | 'grid'
    | 'activity'
    | 'clock'
    | 'check'
    | 'settings'
    | 'help'
    | 'folder'
    | 'link'
    | 'search'
    | 'more'
    | 'close'
    | 'stop'
    | 'retry'
    | 'video'
    | 'music'
    | 'sun'
    | 'moon'
    | 'system'
    | 'chevron'
    | 'trash'
    | 'alert';
  let { name, size = 20 }: { name: IconName; size?: number } = $props();
  const paths: Record<IconName, string> = {
    download: 'M12 3v12m-5-5 5 5 5-5M4 16v5h16v-5',
    grid: 'M3 3h6v6H3zM14 3h7M14 8h7M3 14h6v7H3zM14 15h7M14 20h7',
    activity: 'M12 3a9 9 0 1 1-6.36 2.64',
    clock: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18M12 7v5l3 2',
    check: 'm5 12 4 4L19 6',
    settings:
      'm9 3 1-2h4l1 2 3 2 2-.2 2 3-1 2v4l1 2-2 3-2-.2-3 2-1 2h-4l-1-2-3-2-2 .2-2-3 1-2v-4L2 8l2-3 2 .2 3-2M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8',
    help: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18M9.5 9a2.5 2.5 0 1 1 4.3 1.7c-1.1.7-1.8 1-1.8 2.3M12 17h.01',
    folder: 'M3 7V4h6l2 3h10v13H3V7Z',
    link: 'm10 13 4-4M8 15l-1 1a3.5 3.5 0 0 1-5-5l4-4a3.5 3.5 0 0 1 5 0m2 2 1-1a3.5 3.5 0 0 1 5 5l-4 4a3.5 3.5 0 0 1-5 0',
    search: 'M10.5 3a7.5 7.5 0 1 0 0 15 7.5 7.5 0 0 0 0-15M16 16l5 5',
    more: 'M5 12h.01M12 12h.01M19 12h.01',
    close: 'm6 6 12 12M6 18 18 6',
    stop: 'M7 7h10v10H7z',
    retry: 'M3 10a9 9 0 1 1 1 7M3 4v6h6',
    video: 'M3 5h18v14H3zM10 9l5 3-5 3V9Z',
    music: 'M9 18V5l11-2v13M9 18a3 3 0 1 1-3-3h3M20 16a3 3 0 1 1-3-3h3',
    sun: 'M12 8a4 4 0 1 0 0 8 4 4 0 0 0 0-8M12 2v2m0 16v2M2 12h2m16 0h2M5 5l1 1m12 12 1 1M5 19l1-1M18 6l1-1',
    moon: 'M20.5 14a9 9 0 0 1-10.5-10.5A9 9 0 1 0 20.5 14Z',
    system: 'M3 4h18v13H3zM8 21h8M12 17v4',
    chevron: 'm7 10 5 5 5-5',
    trash: 'M3 6h18M9 6V3h6v3M5 6l1 15h12l1-15M10 10v7m4-7v7',
    alert: 'M12 3 2 21h20L12 3ZM12 9v5m0 3h.01'
  };
</script>

<svg
  class="icon"
  width={size}
  height={size}
  viewBox="0 0 24 24"
  fill="none"
  stroke="currentColor"
  stroke-width={name === 'more' ? 3.4 : 1.65}
  stroke-linecap="round"
  stroke-linejoin="round"
  aria-hidden="true"><path d={paths[name]} /></svg
>
