import type { QueueItem } from './frontend-types';
import type { FilenameEditorState } from './filename-editor';

export interface QueueRowActions {
  select: (id: string, selected: boolean) => void;
  details: (id: string) => void;
  quality: (item: QueueItem, value: string) => Promise<void>;
  format: (item: QueueItem, value: string) => Promise<void>;
  download: (item: QueueItem) => Promise<void>;
  cancel: (item: QueueItem) => Promise<void>;
  retry: (item: QueueItem) => Promise<void>;
  reveal: (item: QueueItem) => Promise<void>;
}
export interface FilenameEditorActions {
  state: FilenameEditorState;
  begin: (item: QueueItem) => Promise<void>;
  commit: (id: string) => Promise<boolean>;
  cancel: () => void;
  setDraft: (draft: string) => void;
}
