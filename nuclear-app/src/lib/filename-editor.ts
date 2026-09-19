import type { QueueItem } from './frontend-types';
import {
  canEditFilename,
  getQueueItemDisplayTitle,
  sanitizeFilenameDraft
} from './queue-presentation-helpers';
import type { UiErrorReporter } from './ui-error-reporter';

export function createFilenameEditorState() {
  return { itemId: null as string | null, draft: '', error: '', saving: false };
}
export type FilenameEditorState = ReturnType<typeof createFilenameEditorState>;
export interface FilenameEditorOptions {
  getItems: () => readonly QueueItem[];
  save: (itemId: string, filename: string | null) => Promise<void>;
  isActive: () => boolean;
  errors: UiErrorReporter;
}

export class FilenameEditorController {
  private generation = 0;
  private pending: { generation: number; draft: string; promise: Promise<boolean> } | null = null;

  constructor(
    readonly state: FilenameEditorState,
    private readonly options: FilenameEditorOptions
  ) {}

  async begin(item: QueueItem): Promise<void> {
    if (!this.options.isActive() || !canEditFilename(item)) return;
    if (this.state.itemId === item.id) return;
    if (this.state.itemId && !(await this.commit())) return;
    if (!this.options.isActive()) return;
    this.generation += 1;
    this.state.itemId = item.id;
    this.state.draft = getQueueItemDisplayTitle(item);
    this.state.error = '';
    this.state.saving = false;
    this.options.errors.resolve('filename');
  }

  setDraft(draft: string): void {
    this.state.draft = draft;
  }

  commit(id = this.state.itemId): Promise<boolean> {
    if (!id || id !== this.state.itemId) return Promise.resolve(true);
    const item = this.options.getItems().find((candidate) => candidate.id === id);
    if (!item) {
      this.cancel();
      return Promise.resolve(false);
    }
    const generation = this.generation;
    const draft = this.state.draft;
    if (this.pending?.generation === generation) {
      if (this.pending.draft === draft) return this.pending.promise;
      return this.pending.promise.then(() =>
        this.owns(generation, id, draft) ? this.commit(id) : false
      );
    }
    const attempt = this.options.errors.begin('filename', 'Renaming a download');
    const cleaned = sanitizeFilenameDraft(draft);
    if (!cleaned || !canEditFilename(item)) {
      this.state.error = attempt.fail(
        !cleaned
          ? 'Filename must contain at least one valid character.'
          : 'This download has already started. Its filename can no longer be changed.'
      );
      return Promise.resolve(false);
    }
    this.state.error = '';
    this.state.saving = true;
    const promise = this.save(item, cleaned, generation, draft, attempt.fail);
    this.pending = { generation, draft, promise };
    void promise.then(() => {
      if (this.pending?.promise === promise) this.pending = null;
    });
    return promise;
  }

  async flushFor(ids: readonly string[]): Promise<boolean> {
    while (this.state.itemId && ids.includes(this.state.itemId)) {
      if (!(await this.commit()) || !this.options.isActive()) return false;
    }
    return true;
  }

  cancel(): void {
    this.generation += 1;
    this.state.itemId = null;
    this.state.draft = '';
    this.state.error = '';
    this.state.saving = false;
    this.options.errors.resolve('filename');
  }

  private async save(
    item: QueueItem,
    cleaned: string,
    generation: number,
    draft: string,
    fail: (error: unknown) => string
  ): Promise<boolean> {
    try {
      await this.options.save(item.id, cleaned !== item.title ? cleaned : null);
      if (!this.options.isActive()) return false;
      if (this.owns(generation, item.id, draft)) this.cancel();
      return true;
    } catch (error) {
      if (this.options.isActive() && this.owns(generation, item.id, draft))
        this.state.error = fail(error);
      return false;
    } finally {
      if (this.options.isActive() && this.generation === generation) this.state.saving = false;
    }
  }

  private owns(generation: number, id: string, draft: string): boolean {
    return this.generation === generation && this.state.itemId === id && this.state.draft === draft;
  }
}
