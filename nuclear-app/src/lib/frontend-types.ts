import type { CookieConfig as BackendCookieConfig } from './bindings/CookieConfig';
import type { PlaylistEntry } from './bindings/PlaylistEntry';
import type { PlaylistInfo } from './bindings/PlaylistInfo';
import type { EventMap } from './ipc-client';

export type DownloadStatus =
  | 'fetching'
  | 'ready'
  | 'queued'
  | 'downloading'
  | 'postprocessing'
  | 'cancelling'
  | 'completed'
  | 'error'
  | 'cancelled';
export type DownloadPhase =
  'download' | 'postprocess' | 'waiting_conversion' | 'conversion' | 'complete';
export type CookieMode = 'browser' | 'file';
export const supportedBrowsers = [
  'firefox',
  'chrome',
  'edge',
  'brave',
  'opera',
  'chromium'
] as const;
export type BrowserName = (typeof supportedBrowsers)[number];
export const videoFormats = ['mp4', 'mkv', 'webm'] as const;
export const audioFormats = ['mp3', 'flac', 'wav', 'aac', 'opus'] as const;
export type VideoFormat = (typeof videoFormats)[number];
export type AudioFormat = (typeof audioFormats)[number];
export type OutputFormat = VideoFormat | AudioFormat;

export type CookieConfig = Omit<BackendCookieConfig, 'mode' | 'browser'> & {
  mode: CookieMode;
  browser: BrowserName;
};

export interface PlaylistModalEntry extends PlaylistEntry {
  selected: boolean;
}

export interface PlaylistModal {
  info: PlaylistInfo;
  inspectionOperationId: string;
  url: string;
  cookieConfig: CookieConfig | null;
  entries: PlaylistModalEntry[];
}

export interface QueueItem {
  id: string;
  downloadId: string | null;
  url: string;
  title: string;
  customFilename: string | null;
  duration: number | null;
  channel: string | null;
  thumbnail: string | null;
  infoLoaded: boolean;
  hasAudio: boolean | null;
  status: DownloadStatus;
  quality: string;
  format: OutputFormat;
  cookieConfig: CookieConfig | null;
  availableQualities: string[];
  progress: number;
  downloadProgress: number;
  conversionProgress: number | null;
  phase: DownloadPhase | null;
  speed: string;
  eta: string;
  error: string | null;
  errorCode: string | null;
  errorDetail: string | null;
  diagnosticsOpen: boolean;
  filename: string | null;
  selected: boolean;
}

export type DownloadProgressPayload = EventMap['download-progress'];
export type DownloaderRuntimeUpdateProgressPayload = EventMap['downloader-runtime-update-progress'];
export type UpdateInstallProgressPayload = EventMap['update-install-progress'];
