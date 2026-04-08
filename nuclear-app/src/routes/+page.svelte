<script lang="ts">
  import { invoke } from "@tauri-apps/api/core";
  import { listen } from "@tauri-apps/api/event";
  import { open } from "@tauri-apps/plugin-dialog";
  import { onMount, tick } from "svelte";

  type DownloadStatus =
    | "fetching"
    | "ready"
    | "queued"
    | "downloading"
    | "postprocessing"
    | "completed"
    | "error"
    | "cancelled";

  type CookieMode = "browser" | "file";

  const supportedBrowsers = [
    "firefox",
    "chrome",
    "edge",
    "brave",
    "opera",
    "chromium",
  ] as const;
  type BrowserName = (typeof supportedBrowsers)[number];

  const videoFormats = ["mp4", "mkv", "webm"] as const;
  const audioFormats = ["mp3", "flac", "wav", "aac", "opus"] as const;
  type VideoFormat = (typeof videoFormats)[number];
  type AudioFormat = (typeof audioFormats)[number];
  type OutputFormat = VideoFormat | AudioFormat;
  const MAX_PARALLEL_DOWNLOADS = 5;

  interface CookieConfig {
    enabled: boolean;
    mode: CookieMode;
    browser: BrowserName;
    cookie_file: string | null;
  }

  interface VideoInfo {
    id: string;
    title: string;
    duration: number | null;
    channel: string | null;
    thumbnail: string | null;
    url: string;
    available_qualities: string[];
    has_audio: boolean;
  }

  interface PlaylistEntry {
    id: string;
    title: string | null;
    duration: number | null;
    url: string;
    thumbnail: string | null;
  }

  interface PlaylistModalEntry extends PlaylistEntry {
    selected: boolean;
  }

  interface PlaylistInfo {
    title: string;
    channel: string | null;
    entry_count: number;
    entries: PlaylistEntry[];
  }

  interface PlaylistModal {
    info: PlaylistInfo;
    url: string;
    entries: PlaylistModalEntry[];
  }

  interface QueueItem {
    id: string;
    downloadId: string | null;
    url: string;
    title: string;
    customFilename: string | null;
    duration: number | null;
    channel: string | null;
    thumbnail: string | null;
    status: DownloadStatus;
    quality: string;
    format: OutputFormat;
    cookieConfig: CookieConfig | null;
    availableQualities: string[];
    progress: number;
    speed: string;
    eta: string;
    error: string | null;
    filename: string | null;
    selected: boolean;
  }

  interface DownloadRequest {
    url: string;
    quality: string;
    format: OutputFormat;
    output_dir: string;
    cookie_config: CookieConfig | null;
    filename_override: string | null;
  }

  interface DownloadProgressPayload {
    download_id: string;
    status: DownloadStatus;
    progress: number;
    speed: string | null;
    eta: string | null;
    error: string | null;
    filename: string | null;
  }

  function isActiveStatus(status: DownloadStatus): boolean {
    return status === "downloading" || status === "postprocessing";
  }

  function isEditablePendingStatus(status: DownloadStatus): boolean {
    return status === "ready" || status === "queued";
  }

  function buildQueueSummary(items: QueueItem[]) {
    const counts = {
      total: items.length,
      ready: 0,
      downloading: 0,
      completed: 0,
      failed: 0,
    };

    let hasReady = false;
    let hasSelectedReady = false;
    let hasActive = false;
    let hasCompleted = false;
    let hasSelected = false;

    for (const item of items) {
      if (item.status === "ready") {
        counts.ready += 1;
        hasReady = true;
        if (item.selected) {
          hasSelectedReady = true;
        }
      }

      if (isActiveStatus(item.status)) {
        counts.downloading += 1;
        hasActive = true;
      }

      if (item.status === "completed") {
        counts.completed += 1;
        hasCompleted = true;
      } else if (item.status === "cancelled") {
        hasCompleted = true;
      } else if (item.status === "error") {
        counts.failed += 1;
      }

      if (item.selected) {
        hasSelected = true;
      }
    }

    return {
      counts,
      hasReady,
      hasSelectedReady,
      hasActive,
      hasCompleted,
      hasSelected,
    };
  }

  // -- State --
  let urlInput = $state("");
  let outputDir = $state("");
  let globalQuality = $state("best");
  let globalFormat = $state<OutputFormat>("mp4");
  let queue = $state<QueueItem[]>([]);
  let ytdlpVersion = $state<string | null>(null);
  let ffmpegAvailable = $state(false);
  let urlError = $state("");
  let useCookies = $state(false);
  let cookieMode = $state<CookieMode>("browser");
  let cookieBrowser = $state<BrowserName>("firefox");
  let cookieFilePath = $state("");
  let playlistModal = $state<PlaylistModal | null>(null);
  let playlistLoading = $state(false);
  let editingTitleId = $state<string | null>(null);
  let editingTitleDraft = $state("");
  let titleEditorInput = $state<HTMLInputElement | null>(null);
  let pendingDownloadIds = $state<string[]>([]);
  let priorityDownloadId = $state<string | null>(null);
  let schedulerRunning = false;

  // -- Lifecycle --
  onMount(() => {
    let unlistenProgress: (() => void) | undefined;

    const setup = async () => {
      try {
        ytdlpVersion = await invoke<string>("check_ytdlp");
      } catch {
        ytdlpVersion = null;
      }

      try {
        ffmpegAvailable = await invoke<boolean>("check_ffmpeg");
      } catch {
        ffmpegAvailable = false;
      }

      try {
        outputDir = await invoke<string>("default_download_dir");
      } catch {
        outputDir = "";
      }

      unlistenProgress = await listen<DownloadProgressPayload>(
        "download-progress",
        (event) => {
          const progress = event.payload;
          const idx = queue.findIndex(
            (item) => item.downloadId === progress.download_id
          );

          if (idx === -1) return;

          queue[idx] = {
            ...queue[idx],
            status: progress.status,
            downloadId:
              progress.status === "completed" ||
              progress.status === "error" ||
              progress.status === "cancelled"
                ? null
                : queue[idx].downloadId,
            progress: progress.progress,
            speed: progress.status === "completed" || progress.status === "error" || progress.status === "cancelled"
              ? ""
              : progress.speed ?? "",
            eta: progress.status === "completed" || progress.status === "error" || progress.status === "cancelled"
              ? ""
              : progress.eta ?? "",
            error: progress.error ? normalizeDownloadError(progress.error) : null,
            filename: progress.filename ?? queue[idx].filename,
          };

          if (
            progress.status === "completed" ||
            progress.status === "error" ||
            progress.status === "cancelled"
          ) {
            void pumpDownloadQueue();
          }
        }
      );
    };

    void setup();

    return () => {
      unlistenProgress?.();
    };
  });

  // -- Helpers --
  function genId(): string {
    return crypto.randomUUID();
  }

  function formatDuration(seconds: number | null | undefined): string {
    if (!seconds) return "--:--";

    const h = Math.floor(seconds / 3600);
    const m = Math.floor((seconds % 3600) / 60);
    const s = Math.floor(seconds % 60);

    if (h > 0) {
      return `${h}:${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
    }

    return `${m}:${String(s).padStart(2, "0")}`;
  }

  function isPlaylistUrl(url: string): boolean {
    return /[?&]list=/.test(url) || /\/playlist\?/.test(url);
  }

  function normalizeDownloadError(message: string): string {
    if (
      /saml|oauth|microsoftonline|okta|shibboleth/i.test(message) ||
      (/Unsupported URL/i.test(message) && /login|auth|sign.?in/i.test(message))
    ) {
      return "This site requires login. Enable Cookies (use Firefox or a cookies.txt file) and make sure you're logged in.";
    }

    if (/[Cc]ould not copy.*cookie|cookie.*database/i.test(message)) {
      return "Browser cookie database is locked. Close your browser first, or switch to Firefox/cookie file mode.";
    }

    return message;
  }

  function getCookieConfig(): CookieConfig | null {
    if (!useCookies) return null;

    return {
      enabled: true,
      mode: cookieMode,
      browser: cookieBrowser,
      cookie_file: cookieMode === "file" ? cookieFilePath || null : null,
    };
  }

  function pickFirstPath(selection: string | string[] | null): string | null {
    if (typeof selection === "string") return selection;
    if (Array.isArray(selection)) return selection[0] ?? null;
    return null;
  }

  function getQueueUrls(): Set<string> {
    return new Set(queue.map((item) => item.url));
  }

  function getActiveDownloadCount(): number {
    let activeCount = 0;
    for (const item of queue) {
      if (isActiveStatus(item.status)) {
        activeCount += 1;
      }
    }
    return activeCount;
  }

  function enqueueItems(itemIds: string[], prioritize = false): void {
    const enqueueIds: string[] = [];

    for (const itemId of itemIds) {
      const item = queue.find((queueItem) => queueItem.id === itemId);
      if (!item || !isEditablePendingStatus(item.status) || enqueueIds.includes(itemId)) {
        continue;
      }

      enqueueIds.push(itemId);
    }

    if (enqueueIds.length === 0) return;

    const queuedIds = new Set(enqueueIds);
    queue = queue.map((item) =>
      queuedIds.has(item.id) && item.status === "ready"
        ? { ...item, status: "queued" }
        : item
    );

    const knownPendingIds = new Set(pendingDownloadIds);
    pendingDownloadIds = [
      ...pendingDownloadIds,
      ...enqueueIds.filter((itemId) => !knownPendingIds.has(itemId)),
    ];

    if (prioritize) {
      priorityDownloadId = enqueueIds[0] ?? priorityDownloadId;
    }

    void pumpDownloadQueue();
  }

  async function startDownloadForItemId(itemId: string): Promise<boolean> {
    const idx = queue.findIndex((queueItem) => queueItem.id === itemId);
    if (idx === -1 || !isEditablePendingStatus(queue[idx].status)) {
      return false;
    }

    const downloadId = genId();
    const request: DownloadRequest = {
      url: queue[idx].url,
      quality: queue[idx].quality,
      format: queue[idx].format,
      output_dir: outputDir,
      cookie_config: queue[idx].cookieConfig,
      filename_override: queue[idx].customFilename,
    };

    queue[idx] = {
      ...queue[idx],
      downloadId,
      status: "downloading",
      error: null,
      progress: 0,
      speed: "",
      eta: "",
    };

    try {
      await invoke("start_download", { downloadId, request });
      return true;
    } catch (error) {
      const currentIdx = queue.findIndex((queueItem) => queueItem.id === itemId);
      if (currentIdx !== -1) {
        queue[currentIdx] = {
          ...queue[currentIdx],
          downloadId: null,
          status: "error",
          speed: "",
          eta: "",
          error: normalizeDownloadError(String(error)),
        };
      }

      return false;
    }
  }

  async function pumpDownloadQueue(): Promise<void> {
    if (schedulerRunning) return;

    schedulerRunning = true;

    try {
      while (
        pendingDownloadIds.length > 0 &&
        getActiveDownloadCount() < MAX_PARALLEL_DOWNLOADS
      ) {
        const prioritizedItemStillQueued =
          priorityDownloadId &&
          pendingDownloadIds.includes(priorityDownloadId) &&
          queue.some(
            (item) =>
              item.id === priorityDownloadId &&
              isEditablePendingStatus(item.status)
          )
            ? priorityDownloadId
            : null;

        if (priorityDownloadId && !prioritizedItemStillQueued) {
          priorityDownloadId = null;
        }

        const nextId = prioritizedItemStillQueued ?? pendingDownloadIds[0];

        if (nextId === editingTitleId) {
          break;
        }

        pendingDownloadIds = pendingDownloadIds.filter((itemId) => itemId !== nextId);
        if (priorityDownloadId === nextId) {
          priorityDownloadId = null;
        }
        await startDownloadForItemId(nextId);
      }
    } finally {
      schedulerRunning = false;

      if (
        pendingDownloadIds.length > 0 &&
        getActiveDownloadCount() < MAX_PARALLEL_DOWNLOADS &&
        pendingDownloadIds[0] !== editingTitleId
      ) {
        queueMicrotask(() => {
          void pumpDownloadQueue();
        });
      }
    }
  }

  function resolveQualitySelection(
    requestedQuality: string,
    availableQualities: string[]
  ): string {
    return availableQualities.includes(requestedQuality)
      ? requestedQuality
      : "best";
  }

  function getQueueItemDisplayTitle(item: QueueItem): string {
    return item.customFilename ?? item.title;
  }

  function sanitizeFilenameDraft(value: string): string {
    let cleaned = value.trim();
    if (!cleaned) return "";

    cleaned = cleaned.replace(/[<>:"/\\|?*\u0000-\u001F]/g, "_");

    for (const extension of [...videoFormats, ...audioFormats]) {
      const suffix = `.${extension}`;
      if (cleaned.toLowerCase().endsWith(suffix)) {
        cleaned = cleaned.slice(0, -suffix.length);
        break;
      }
    }

    cleaned = cleaned.trim().replace(/[. ]+$/g, "");
    return cleaned;
  }

  function canEditFilename(item: QueueItem): boolean {
    return isEditablePendingStatus(item.status);
  }

  async function beginFilenameEdit(item: QueueItem): Promise<void> {
    if (!canEditFilename(item)) return;

    if (editingTitleId && editingTitleId !== item.id) {
      commitFilenameEdit(editingTitleId);
    }

    editingTitleId = item.id;
    editingTitleDraft = getQueueItemDisplayTitle(item);

    await tick();
    titleEditorInput?.focus();
    titleEditorInput?.select();
  }

  function commitFilenameEdit(itemId: string | null = editingTitleId): void {
    if (!itemId) return;

    const idx = queue.findIndex((item) => item.id === itemId);
    if (idx === -1) {
      editingTitleId = null;
      editingTitleDraft = "";
      titleEditorInput = null;
      return;
    }

    const cleaned = sanitizeFilenameDraft(editingTitleDraft);
    const customFilename =
      cleaned && cleaned !== queue[idx].title ? cleaned : null;

    queue[idx] = {
      ...queue[idx],
      customFilename,
    };

    editingTitleId = null;
    editingTitleDraft = "";
    titleEditorInput = null;
    void pumpDownloadQueue();
  }

  function cancelFilenameEdit(): void {
    editingTitleId = null;
    editingTitleDraft = "";
    titleEditorInput = null;
    void pumpDownloadQueue();
  }

  function handleFilenameEditorKeydown(event: KeyboardEvent): void {
    if (event.key === "Enter") {
      event.preventDefault();
      commitFilenameEdit();
    } else if (event.key === "Escape") {
      event.preventDefault();
      cancelFilenameEdit();
    }
  }

  function closePlaylistModal(): void {
    playlistModal = null;
  }

  function handleWindowKeydown(event: KeyboardEvent): void {
    if (event.key === "Escape" && playlistModal) {
      closePlaylistModal();
    }
  }

  function handleQueueSelectionChange(event: Event): void {
    const checked = (event.currentTarget as HTMLInputElement).checked;
    queue = queue.map((item) => ({ ...item, selected: checked }));
  }

  function handlePlaylistSelectionToggle(event: Event): void {
    const checked = (event.currentTarget as HTMLInputElement).checked;
    toggleAllPlaylist(checked);
  }

  async function browseCookieFile(): Promise<void> {
    const file = pickFirstPath(
      await open({
        filters: [{ name: "Cookie Files", extensions: ["txt"] }],
      })
    );

    if (file) cookieFilePath = file;
  }

  async function browseOutputDir(): Promise<void> {
    const dir = pickFirstPath(await open({ directory: true }));
    if (dir) outputDir = dir;
  }

  // -- Actions --
  async function addToQueue(): Promise<void> {
    urlError = "";
    const url = urlInput.trim();
    if (!url) return;

    const valid = await invoke<boolean>("validate_url", { url });
    if (!valid) {
      urlError = "Please enter a valid URL (must start with http:// or https://)";
      return;
    }

    if (isPlaylistUrl(url)) {
      urlInput = "";
      playlistLoading = true;

      try {
        const info = await invoke<PlaylistInfo>("fetch_playlist_info", {
          url,
          cookieConfig: getCookieConfig(),
        });

        playlistModal = {
          info,
          url,
          entries: info.entries.map((entry) => ({ ...entry, selected: true })),
        };
      } catch (error) {
        urlError = "Failed to load playlist: " + String(error);
      } finally {
        playlistLoading = false;
      }

      return;
    }

    if (getQueueUrls().has(url)) {
      urlError = "URL already in queue";
      return;
    }

    addSingleVideo(url);
  }

  function addSingleVideo(url: string): void {
    const item: QueueItem = {
      id: genId(),
      downloadId: null,
      url,
      title: "Fetching info...",
      customFilename: null,
      duration: null,
      channel: null,
      thumbnail: null,
      status: "fetching",
      quality: globalQuality,
      format: globalFormat,
      cookieConfig: getCookieConfig(),
      availableQualities: [],
      progress: 0,
      speed: "",
      eta: "",
      error: null,
      filename: null,
      selected: false,
    };

    queue = [...queue, item];
    void fetchVideoInfoForItem(item.id, url, item.cookieConfig);
  }

  async function fetchVideoInfoForItem(
    itemId: string,
    url: string,
    cookieConfig: CookieConfig | null
  ): Promise<void> {
    try {
      const info = await invoke<VideoInfo>("fetch_video_info", {
        url,
        cookieConfig,
      });

      const idx = queue.findIndex((item) => item.id === itemId);
      if (idx === -1) return;

      const availableQualities = ["best", ...info.available_qualities];

      queue[idx] = {
        ...queue[idx],
        title: info.title,
        duration: info.duration,
        channel: info.channel,
        thumbnail: info.thumbnail,
        availableQualities,
        quality: resolveQualitySelection(queue[idx].quality, availableQualities),
        status: "ready",
      };
    } catch (error) {
      const idx = queue.findIndex((item) => item.id === itemId);
      if (idx === -1) return;

      queue[idx] = {
        ...queue[idx],
        title: "Error",
        status: "error",
        error: normalizeDownloadError(String(error)),
      };
    }
  }

  function addPlaylistSelection(): void {
    const modal = playlistModal;
    if (!modal) return;

    const selectedEntries = modal.entries.filter((entry) => entry.selected);
    const queuedUrls = getQueueUrls();
    urlInput = "";
    closePlaylistModal();

    for (const entry of selectedEntries) {
      if (!queuedUrls.has(entry.url)) {
        addSingleVideo(entry.url);
        queuedUrls.add(entry.url);
      }
    }
  }

  function toggleAllPlaylist(checked: boolean): void {
    const modal = playlistModal;
    if (!modal) return;

    playlistModal = {
      ...modal,
      entries: modal.entries.map((entry) => ({
        ...entry,
        selected: checked,
      })),
    };
  }

  async function downloadItem(item: QueueItem): Promise<void> {
    const idx = queue.findIndex((queueItem) => queueItem.id === item.id);
    if (idx === -1 || !isEditablePendingStatus(queue[idx].status)) return;

    if (
      pendingDownloadIds.length === 0 &&
      getActiveDownloadCount() < MAX_PARALLEL_DOWNLOADS &&
      !schedulerRunning
    ) {
      await startDownloadForItemId(item.id);
      return;
    }

    enqueueItems([item.id], true);
  }

  async function downloadAll(): Promise<void> {
    const readyIds = queue
      .filter((item) => item.status === "ready")
      .map((item) => item.id);
    enqueueItems(readyIds);
  }

  async function downloadSelected(): Promise<void> {
    const selectedIds = queue
      .filter(
      (item) => item.selected && item.status === "ready"
    )
      .map((item) => item.id);
    enqueueItems(selectedIds);
  }

  async function cancelItem(item: QueueItem): Promise<void> {
    if (!item.downloadId) return;

    try {
      await invoke("cancel_download", { downloadId: item.downloadId });
    } catch {
      // already done
    }
  }

  async function cancelAll(): Promise<void> {
    const active = queue.filter(
      (item) =>
        item.status === "downloading" || item.status === "postprocessing"
    );

    for (const item of active) {
      await cancelItem(item);
    }
  }

  function removeSelected(): void {
    const removableIds = new Set(
      queue
        .filter((item) => item.selected && !isActiveStatus(item.status))
        .map((item) => item.id)
    );

    if (removableIds.size === 0) return;

    queue = queue.filter((item) => !removableIds.has(item.id));
    pendingDownloadIds = pendingDownloadIds.filter((itemId) => !removableIds.has(itemId));
    if (priorityDownloadId && removableIds.has(priorityDownloadId)) {
      priorityDownloadId = null;
    }

    if (editingTitleId && removableIds.has(editingTitleId)) {
      cancelFilenameEdit();
    }
  }

  function clearCompleted(): void {
    queue = queue.filter(
      (item) => item.status !== "completed" && item.status !== "cancelled"
    );
  }

  function applyGlobalQuality(): void {
    queue = queue.map((item) =>
      isEditablePendingStatus(item.status)
        ? { ...item, quality: globalQuality }
        : item
    );
  }

  function applyGlobalFormat(): void {
    queue = queue.map((item) =>
      isEditablePendingStatus(item.status)
        ? { ...item, format: globalFormat }
        : item
    );
  }

  function handleUrlKeydown(event: KeyboardEvent): void {
    if (event.key === "Enter") {
      void addToQueue();
    }
  }

  // -- Derived --
  let queueSummary = $derived(buildQueueSummary(queue));
</script>

<svelte:window onkeydown={handleWindowKeydown} />

<main>
  <!-- Header -->
  <header>
    <h1>Nuclear Downloader</h1>
    <div class="status-badges">
      {#if ytdlpVersion}
        <span class="badge ok">yt-dlp {ytdlpVersion}</span>
      {:else}
        <span class="badge err">yt-dlp not found</span>
      {/if}
      {#if ffmpegAvailable}
        <span class="badge ok">FFmpeg</span>
      {:else}
        <span class="badge warn">No FFmpeg</span>
      {/if}
    </div>
  </header>

  <!-- URL Input -->
  <section class="url-bar">
    <input
      type="text"
      placeholder="Paste a video URL..."
      bind:value={urlInput}
      onkeydown={handleUrlKeydown}
      class:input-error={Boolean(urlError)}
    />
    <button class="primary" onclick={addToQueue} disabled={!ytdlpVersion || playlistLoading}>
      {playlistLoading ? "Loading..." : "Add"}
    </button>
    {#if urlError}
      <span class="error-text">{urlError}</span>
    {/if}
  </section>

  <!-- Settings Row -->
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
          {#each videoFormats as fmt}
            <option value={fmt}>{fmt.toUpperCase()}</option>
          {/each}
        </optgroup>
        <optgroup label="Audio Only">
          {#each audioFormats as fmt}
            <option value={fmt}>{fmt.toUpperCase()}</option>
          {/each}
        </optgroup>
      </select>
    </div>
    <div class="setting output-dir">
      <label for="outdir">Output</label>
      <input id="outdir" type="text" bind:value={outputDir} readonly />
      <button onclick={browseOutputDir}>Browse</button>
    </div>
    <div class="setting cookie-setting">
      <label>
        <input type="checkbox" bind:checked={useCookies} />
        Cookies
      </label>
      {#if useCookies}
        <select bind:value={cookieMode} class="cookie-mode-select">
          <option value="browser">From Browser</option>
          <option value="file">From File</option>
        </select>
        {#if cookieMode === "browser"}
          <select bind:value={cookieBrowser}>
            {#each supportedBrowsers as b}
              <option value={b}>{b.charAt(0).toUpperCase() + b.slice(1)}</option>
            {/each}
          </select>
          {#if cookieBrowser === "chrome" || cookieBrowser === "edge" || cookieBrowser === "brave" || cookieBrowser === "chromium"}
            <span class="cookie-warn-clean">Chromium browsers block cookie access; use Firefox or a cookie file instead</span>
            <span class="cookie-warn">Chromium browsers block cookie access — use Firefox or a cookie file instead</span>
          {:else}
            <span class="cookie-hint">Close {cookieBrowser} first if errors occur</span>
          {/if}
        {:else}
          <button class="cookie-browse" onclick={browseCookieFile}>
            {cookieFilePath ? cookieFilePath.split(/[\\/]/).pop() : "Select cookies.txt"}
          </button>
          <span class="cookie-hint">Export via browser extension (e.g. "Get cookies.txt LOCALLY")</span>
        {/if}
      {/if}
    </div>
  </section>

  <!-- Action Buttons -->
  <section class="actions">
    <button class="primary" onclick={downloadAll} disabled={!queueSummary.hasReady}>Download All</button>
    <button onclick={downloadSelected} disabled={!queueSummary.hasSelectedReady}>Download Selected</button>
    <button onclick={removeSelected} disabled={!queueSummary.hasSelected}>Remove Selected</button>
    <button onclick={clearCompleted} disabled={!queueSummary.hasCompleted}>Clear Done</button>
    <button class="danger" onclick={cancelAll} disabled={!queueSummary.hasActive}>Cancel All</button>
  </section>

  <!-- Queue Table -->
  <section class="queue">
    {#if queue.length === 0}
      <div class="empty-state">
        <p>No videos in queue. Paste a video URL above to get started.</p>
      </div>
    {:else}
      <table>
        <thead>
          <tr>
            <th class="col-check">
              <input
                type="checkbox"
                onchange={handleQueueSelectionChange}
              />
            </th>
            <th class="col-title">Title</th>
            <th class="col-status">Status</th>
            <th class="col-quality">Quality</th>
            <th class="col-format">Format</th>
            <th class="col-progress">Progress</th>
            <th class="col-speed">Speed</th>
            <th class="col-eta">ETA</th>
            <th class="col-actions"></th>
          </tr>
        </thead>
        <tbody>
          {#each queue as item, i (item.id)}
            <tr class="queue-item" class:downloading={item.status === "downloading"}>
              <td class="col-check">
                <input type="checkbox" bind:checked={queue[i].selected} />
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
                    {#if editingTitleId === item.id}
                      <input
                        bind:this={titleEditorInput}
                        bind:value={editingTitleDraft}
                        type="text"
                        class="title-editor"
                        aria-label="Edit queued filename"
                        onblur={() => commitFilenameEdit(item.id)}
                        onkeydown={handleFilenameEditorKeydown}
                        onclick={(event) => event.stopPropagation()}
                      />
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
                    {#if item.channel}
                      <span class="channel">{item.channel}</span>
                    {/if}
                    {#if item.duration}
                      <span class="duration">{formatDuration(item.duration)}</span>
                    {/if}
                  </div>
                </div>
              </td>
              <td class="col-status">
                <span class="status-pill {item.status}">
                  {item.status === "postprocessing" ? "converting" : item.status}
                </span>
                {#if item.error}
                  <span class="error-tooltip" title={item.error}>!</span>
                {/if}
              </td>
              <td class="col-quality">
                {#if isEditablePendingStatus(item.status)}
                  <select bind:value={queue[i].quality}>
                    {#each item.availableQualities as q}
                      <option value={q}>{q === "best" ? "Best" : q}</option>
                    {/each}
                  </select>
                {:else}
                  <span class="muted">{item.quality}</span>
                {/if}
              </td>
              <td class="col-format">
                {#if isEditablePendingStatus(item.status)}
                  <select bind:value={queue[i].format}>
                    <optgroup label="Video">
                      {#each videoFormats as fmt}
                        <option value={fmt}>{fmt.toUpperCase()}</option>
                      {/each}
                    </optgroup>
                    <optgroup label="Audio">
                      {#each audioFormats as fmt}
                        <option value={fmt}>{fmt.toUpperCase()}</option>
                      {/each}
                    </optgroup>
                  </select>
                {:else}
                  <span class="muted">{item.format.toUpperCase()}</span>
                {/if}
              </td>
              <td class="col-progress">
                <div class="progress-bar">
                  <div
                    class="progress-fill"
                    class:complete={item.status === "completed"}
                    class:error={item.status === "error"}
                    style="width: {item.progress}%"
                  ></div>
                  <span class="progress-text">{Math.round(item.progress)}%</span>
                </div>
              </td>
              <td class="col-speed">
                <span class="muted">{item.speed}</span>
              </td>
              <td class="col-eta">
                <span class="muted">{item.eta}</span>
              </td>
              <td class="col-actions">
                {#if isEditablePendingStatus(item.status)}
                  <button class="small primary" onclick={() => downloadItem(item)}>DL</button>
                {:else if item.status === "downloading" || item.status === "postprocessing"}
                  <button class="small danger" onclick={() => cancelItem(item)}>X</button>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </section>

  <!-- Status Bar -->
  <footer>
    <span>{queueSummary.counts.total} items</span>
    <span class="sep">|</span>
    <span>{queueSummary.counts.ready} ready</span>
    <span class="sep">|</span>
    <span>{queueSummary.counts.downloading} downloading</span>
    <span class="sep">|</span>
    <span>{queueSummary.counts.completed} done</span>
    {#if queueSummary.counts.failed > 0}
      <span class="sep">|</span>
      <span class="error-text">{queueSummary.counts.failed} failed</span>
    {/if}
  </footer>
</main>

<!-- Playlist Picker Modal -->
{#if playlistModal}
  <div class="modal-layer">
    <button
      type="button"
      class="modal-backdrop"
      aria-label="Close playlist picker"
      onclick={closePlaylistModal}
    ></button>
    <div
      class="modal"
      role="dialog"
      aria-modal="true"
      aria-labelledby="playlist-modal-title"
    >
      <div class="modal-header">
        <div>
          <h2 id="playlist-modal-title">{playlistModal.info.title}</h2>
          {#if playlistModal.info.channel}
            <span class="modal-channel">{playlistModal.info.channel}</span>
          {/if}
          <span class="modal-count">{playlistModal.info.entry_count} videos</span>
        </div>
        <button class="small" onclick={closePlaylistModal}>Close</button>
      </div>
      <div class="modal-controls">
        <label class="select-all-label">
          <input
            type="checkbox"
            checked={playlistModal.entries.every((entry) => entry.selected)}
            onchange={handlePlaylistSelectionToggle}
          />
          Select All
        </label>
        <span class="muted">
          {playlistModal.entries.filter((entry) => entry.selected).length} of {playlistModal.entries.length} selected
        </span>
      </div>
      <div class="modal-list">
        {#each playlistModal.entries as entry, i (entry.id)}
          <label class="playlist-entry" class:entry-selected={entry.selected}>
            <input
              type="checkbox"
              bind:checked={playlistModal.entries[i].selected}
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
      <div class="modal-footer">
        <button class="primary" onclick={addPlaylistSelection}
          disabled={!playlistModal.entries.some((entry) => entry.selected)}>
          Add {playlistModal.entries.filter((entry) => entry.selected).length} Videos to Queue
        </button>
        <button onclick={closePlaylistModal}>Cancel</button>
      </div>
    </div>
  </div>
{/if}

<style>
  /* -- Catppuccin Mocha Palette -- */
  :root {
    --crust: #11111b;
    --mantle: #181825;
    --base: #1e1e2e;
    --surface0: #313244;
    --surface1: #45475a;
    --surface2: #585b70;
    --overlay0: #6c7086;
    --text: #cdd6f4;
    --subtext0: #a6adc8;
    --subtext1: #bac2de;
    --blue: #89b4fa;
    --green: #a6e3a1;
    --red: #f38ba8;
    --yellow: #f9e2af;
    --mauve: #cba6f7;
    --teal: #94e2d5;
  }

  :global(body) {
    margin: 0;
    padding: 0;
    background: var(--base);
    color: var(--text);
    font-family: "Segoe UI", system-ui, -apple-system, sans-serif;
    font-size: 14px;
    overflow: hidden;
    height: 100vh;
  }

  main {
    display: flex;
    flex-direction: column;
    height: 100vh;
    padding: 0;
  }

  /* Header */
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 20px;
    background: var(--mantle);
    border-bottom: 1px solid var(--surface0);
    -webkit-user-select: none;
    user-select: none;
  }

  header h1 {
    margin: 0;
    font-size: 20px;
    font-weight: 700;
    color: var(--blue);
  }

  .status-badges {
    display: flex;
    gap: 8px;
  }

  .badge {
    font-size: 11px;
    padding: 2px 8px;
    border-radius: 4px;
    font-weight: 500;
  }
  .badge.ok {
    background: color-mix(in srgb, var(--green) 20%, transparent);
    color: var(--green);
  }
  .badge.warn {
    background: color-mix(in srgb, var(--yellow) 20%, transparent);
    color: var(--yellow);
  }
  .badge.err {
    background: color-mix(in srgb, var(--red) 20%, transparent);
    color: var(--red);
  }

  /* URL Bar */
  .url-bar {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 12px 20px;
    background: var(--mantle);
    flex-wrap: wrap;
  }

  .url-bar input {
    flex: 1;
    min-width: 200px;
  }

  .error-text {
    color: var(--red);
    font-size: 12px;
  }

  /* Inputs */
  input[type="text"],
  select {
    background: var(--surface0);
    border: 1px solid var(--surface1);
    color: var(--text);
    padding: 8px 12px;
    border-radius: 6px;
    font-size: 13px;
    outline: none;
    transition: border-color 0.15s;
  }

  input[type="text"]:focus {
    border-color: var(--blue);
  }

  input.input-error {
    border-color: var(--red);
  }

  select {
    padding: 6px 8px;
    cursor: pointer;
  }

  /* Buttons */
  button {
    padding: 8px 16px;
    border: none;
    border-radius: 6px;
    font-size: 13px;
    font-weight: 500;
    cursor: pointer;
    background: var(--surface0);
    color: var(--text);
    transition: background 0.15s;
  }

  button:hover:not(:disabled) {
    background: var(--surface1);
  }

  button:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }

  button.primary {
    background: var(--blue);
    color: var(--crust);
  }
  button.primary:hover:not(:disabled) {
    background: color-mix(in srgb, var(--blue) 85%, white);
  }

  button.danger {
    background: var(--red);
    color: var(--crust);
  }
  button.danger:hover:not(:disabled) {
    background: color-mix(in srgb, var(--red) 85%, white);
  }

  button.small {
    padding: 4px 10px;
    font-size: 12px;
  }

  /* Settings Row */
  .settings-row {
    display: flex;
    align-items: center;
    gap: 16px;
    padding: 10px 20px;
    background: var(--base);
    border-bottom: 1px solid var(--surface0);
    flex-wrap: wrap;
  }

  .setting {
    display: flex;
    align-items: center;
    gap: 6px;
  }

  .setting label {
    font-size: 12px;
    color: var(--subtext0);
    font-weight: 500;
    white-space: nowrap;
  }

  .output-dir {
    flex: 1;
    min-width: 200px;
  }

  .output-dir input {
    flex: 1;
    min-width: 120px;
  }

  .cookie-setting label {
    display: flex;
    align-items: center;
    gap: 4px;
    cursor: pointer;
  }

  /* Actions */
  .actions {
    display: flex;
    gap: 8px;
    padding: 10px 20px;
    flex-wrap: wrap;
  }

  /* Queue */
  .queue {
    flex: 1;
    overflow-y: auto;
    padding: 0;
  }

  .empty-state {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 100%;
    color: var(--overlay0);
    font-size: 15px;
  }

  table {
    width: 100%;
    border-collapse: collapse;
    table-layout: fixed;
  }

  thead {
    position: sticky;
    top: 0;
    z-index: 1;
    background: var(--mantle);
  }

  th {
    padding: 8px 10px;
    text-align: left;
    font-size: 11px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--subtext0);
    border-bottom: 1px solid var(--surface0);
  }

  td {
    padding: 8px 10px;
    border-bottom: 1px solid var(--surface0);
    vertical-align: middle;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }

  tr:hover {
    background: color-mix(in srgb, var(--surface0) 40%, transparent);
  }

  tr.downloading {
    background: color-mix(in srgb, var(--blue) 5%, transparent);
  }

  .col-check { width: 36px; text-align: center; }
  .col-title { width: auto; }
  .col-status { width: 90px; }
  .col-quality { width: 80px; }
  .col-format { width: 80px; }
  .col-progress { width: 130px; }
  .col-speed { width: 85px; }
  .col-eta { width: 65px; }
  .col-actions { width: 50px; }

  /* Title cell */
  .title-cell {
    display: flex;
    align-items: center;
    gap: 10px;
    overflow: hidden;
  }

  .thumb {
    width: 48px;
    height: 36px;
    object-fit: cover;
    border-radius: 4px;
    flex-shrink: 0;
  }

  .title-info {
    display: flex;
    flex-direction: column;
    overflow: hidden;
    min-width: 0;
  }

  .title-text {
    overflow: hidden;
    text-overflow: ellipsis;
    font-weight: 500;
  }

  .title-button {
    padding: 0;
    border: none;
    background: transparent;
    color: inherit;
    text-align: left;
    font: inherit;
    width: 100%;
    min-width: 0;
  }

  .title-button:hover:not(:disabled) {
    background: transparent;
    color: var(--blue);
  }

  .title-button .title-text {
    display: block;
    cursor: text;
  }

  .title-editor {
    width: 100%;
    min-width: 0;
    padding: 4px 6px;
    font-size: 13px;
    font-weight: 500;
    box-sizing: border-box;
  }

  .channel {
    font-size: 11px;
    color: var(--subtext0);
  }

  .duration {
    font-size: 11px;
    color: var(--overlay0);
  }

  /* Status pills */
  .status-pill {
    display: inline-block;
    padding: 2px 8px;
    border-radius: 4px;
    font-size: 11px;
    font-weight: 500;
    text-transform: capitalize;
  }
  .status-pill.fetching { background: color-mix(in srgb, var(--mauve) 20%, transparent); color: var(--mauve); }
  .status-pill.ready { background: color-mix(in srgb, var(--blue) 20%, transparent); color: var(--blue); }
  .status-pill.queued { background: color-mix(in srgb, var(--teal) 14%, transparent); color: var(--teal); }
  .status-pill.downloading { background: color-mix(in srgb, var(--teal) 20%, transparent); color: var(--teal); }
  .status-pill.postprocessing { background: color-mix(in srgb, var(--yellow) 20%, transparent); color: var(--yellow); }
  .status-pill.completed { background: color-mix(in srgb, var(--green) 20%, transparent); color: var(--green); }
  .status-pill.error { background: color-mix(in srgb, var(--red) 20%, transparent); color: var(--red); }
  .status-pill.cancelled { background: color-mix(in srgb, var(--overlay0) 20%, transparent); color: var(--overlay0); }

  .error-tooltip {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 16px;
    height: 16px;
    border-radius: 50%;
    background: var(--red);
    color: var(--crust);
    font-size: 10px;
    font-weight: 700;
    margin-left: 4px;
    cursor: help;
  }

  /* Progress bar */
  .progress-bar {
    position: relative;
    height: 20px;
    background: var(--surface0);
    border-radius: 4px;
    overflow: hidden;
  }

  .progress-fill {
    height: 100%;
    background: var(--blue);
    transition: width 0.3s ease;
    border-radius: 4px;
  }

  .progress-fill.complete {
    background: var(--green);
  }

  .progress-fill.error {
    background: var(--red);
  }

  .progress-text {
    position: absolute;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    font-size: 11px;
    font-weight: 600;
    color: var(--text);
    text-shadow: 0 1px 2px rgba(0, 0, 0, 0.5);
  }

  .muted {
    color: var(--subtext0);
    font-size: 12px;
  }

  /* Inline selects in table */
  td select {
    width: 100%;
    padding: 3px 4px;
    font-size: 12px;
  }

  input[type="checkbox"] {
    accent-color: var(--blue);
    cursor: pointer;
  }

  /* Footer */
  footer {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 8px 20px;
    background: var(--mantle);
    border-top: 1px solid var(--surface0);
    font-size: 12px;
    color: var(--subtext0);
    -webkit-user-select: none;
    user-select: none;
  }

  .sep {
    color: var(--surface2);
  }

  /* Scrollbar */
  .queue::-webkit-scrollbar {
    width: 8px;
  }
  .queue::-webkit-scrollbar-track {
    background: var(--base);
  }
  .queue::-webkit-scrollbar-thumb {
    background: var(--surface1);
    border-radius: 4px;
  }
  .queue::-webkit-scrollbar-thumb:hover {
    background: var(--surface2);
  }

  /* Cookie controls */
  .cookie-mode-select {
    min-width: 110px;
  }

  .cookie-browse {
    font-size: 12px;
    padding: 4px 10px;
    background: var(--surface0);
    color: var(--text);
    border: 1px solid var(--surface1);
    border-radius: 4px;
    cursor: pointer;
    max-width: 180px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .cookie-browse:hover {
    background: var(--surface1);
  }

  .cookie-hint {
    font-size: 10px;
    color: var(--overlay0);
    font-style: italic;
  }

  .cookie-warn {
    display: none;
  }

  .cookie-warn-clean {
    font-size: 10px;
    color: var(--red);
    font-style: italic;
  }

  /* Playlist Modal */
  .modal-layer {
    position: fixed;
    inset: 0;
    display: flex;
    align-items: center;
    justify-content: center;
    z-index: 100;
  }

  .modal-backdrop {
    position: absolute;
    inset: 0;
    background: rgba(0, 0, 0, 0.6);
    border: none;
    border-radius: 0;
    padding: 0;
  }

  .modal-backdrop:hover:not(:disabled),
  .modal-backdrop:focus-visible {
    background: rgba(0, 0, 0, 0.6);
    outline: none;
  }

  .modal {
    position: relative;
    z-index: 1;
    background: var(--base);
    border: 1px solid var(--surface1);
    border-radius: 12px;
    width: min(700px, 90vw);
    max-height: 80vh;
    display: flex;
    flex-direction: column;
    box-shadow: 0 8px 32px rgba(0, 0, 0, 0.4);
  }

  .modal-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    padding: 16px 20px 12px;
    border-bottom: 1px solid var(--surface0);
  }

  .modal-header h2 {
    margin: 0;
    font-size: 16px;
    font-weight: 600;
    color: var(--text);
  }

  .modal-channel {
    font-size: 12px;
    color: var(--subtext0);
    margin-right: 8px;
  }

  .modal-count {
    font-size: 12px;
    color: var(--overlay0);
  }

  .modal-controls {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 8px 20px;
    border-bottom: 1px solid var(--surface0);
  }

  .select-all-label {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 13px;
    cursor: pointer;
    color: var(--subtext0);
    font-weight: 500;
  }

  .modal-list {
    flex: 1;
    overflow-y: auto;
    padding: 4px 0;
  }

  .playlist-entry {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 6px 20px;
    cursor: pointer;
    transition: background 0.1s;
  }

  .playlist-entry:hover {
    background: color-mix(in srgb, var(--surface0) 50%, transparent);
  }

  .playlist-entry.entry-selected {
    background: color-mix(in srgb, var(--blue) 8%, transparent);
  }

  .entry-thumb {
    width: 64px;
    height: 36px;
    object-fit: cover;
    border-radius: 4px;
    flex-shrink: 0;
    background: var(--surface0);
  }

  .entry-info {
    display: flex;
    flex-direction: column;
    min-width: 0;
    flex: 1;
  }

  .entry-title {
    font-size: 13px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .entry-duration {
    font-size: 11px;
    color: var(--overlay0);
  }

  .modal-footer {
    display: flex;
    gap: 8px;
    justify-content: flex-end;
    padding: 12px 20px;
    border-top: 1px solid var(--surface0);
  }

  .modal-list::-webkit-scrollbar {
    width: 8px;
  }
  .modal-list::-webkit-scrollbar-track {
    background: var(--base);
  }
  .modal-list::-webkit-scrollbar-thumb {
    background: var(--surface1);
    border-radius: 4px;
  }
</style>
