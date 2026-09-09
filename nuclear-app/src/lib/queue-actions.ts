import { appErrorDetail, normalizeAppError } from './frontend-errors';
import type { OutputFormat, QueueItem } from './frontend-types';
import type { WorkflowCommands } from './frontend-workflow-ports';
import { resolveAvailableFormat, resolveAvailableQuality } from './queue-logic';
import { canRetryItem, isActiveStatus, isEditablePendingStatus } from './queue-presentation';

export interface QueueActionState {
  queueActionError: string | null;
  cancelAllError: string | null;
}

export function createQueueActionState(): QueueActionState {
  return { queueActionError: null, cancelAllError: null };
}

export interface QueueActionDependencies extends WorkflowCommands {
  getItems: () => readonly QueueItem[];
  replaceItem: (itemId: string, replace: (item: QueueItem) => QueueItem) => void;
  getCanStartDownloads: () => boolean;
  getGlobalQuality: () => string;
  getGlobalFormat: () => OutputFormat;
  clearProgressDisplayState: (itemId: string) => void;
  getEditingTitleId: () => string | null;
  cancelFilenameEdit: () => void;
  reloadAppSnapshot: () => Promise<void>;
}

export class QueueActionsController {
  constructor(
    readonly state: QueueActionState,
    private readonly dependencies: QueueActionDependencies
  ) {}

  async enqueueItems(itemIds: string[], prioritize = false): Promise<void> {
    const uniqueIds = [...new Set(itemIds)];
    if (uniqueIds.length === 0 || !this.dependencies.getCanStartDownloads()) return;
    this.state.queueActionError = null;
    try {
      await this.dependencies.invoke('enqueue_queue_items', {
        itemIds: uniqueIds,
        priority: prioritize ? 'front' : 'normal'
      });
    } catch (error) {
      if (!this.dependencies.isActive()) return;
      this.state.queueActionError = normalizeAppError(error);
    }
  }

  async downloadItem(item: QueueItem): Promise<void> {
    if (!this.dependencies.getCanStartDownloads()) return;
    if (!isEditablePendingStatus(item.status)) return;
    await this.enqueueItems([item.id], true);
  }

  async downloadAll(): Promise<void> {
    if (!this.dependencies.getCanStartDownloads()) return;
    const readyIds = this.dependencies
      .getItems()
      .filter((item) => item.status === 'ready')
      .map((item) => item.id);
    await this.enqueueItems(readyIds);
  }

  async downloadSelected(): Promise<void> {
    if (!this.dependencies.getCanStartDownloads()) return;
    const selectedIds = this.dependencies
      .getItems()
      .filter((item) => item.selected && item.status === 'ready')
      .map((item) => item.id);
    await this.enqueueItems(selectedIds);
  }

  async cancelItem(item: QueueItem): Promise<void> {
    if (!item.downloadId) return;

    const current = this.dependencies.getItems().find((candidate) => candidate.id === item.id);
    if (!current) return;
    const previousStatus = current.status;
    this.dependencies.replaceItem(item.id, (candidate) => ({
      ...candidate,
      status: 'cancelling',
      error: null,
      errorCode: null,
      errorDetail: null
    }));

    try {
      await this.dependencies.invoke('cancel_operation', { operationId: item.downloadId });
    } catch (error) {
      if (!this.dependencies.isActive()) return;
      this.dependencies.replaceItem(item.id, (candidate) =>
        candidate.status === 'cancelling'
          ? {
              ...candidate,
              status: previousStatus,
              error: `Cancellation failed: ${normalizeAppError(error)}`,
              errorCode: 'cancel_failed',
              errorDetail: appErrorDetail(error),
              diagnosticsOpen: true
            }
          : candidate
      );
    }
  }

  async cancelAll(): Promise<void> {
    this.state.cancelAllError = null;
    try {
      const result = await this.dependencies.invoke('cancel_all_downloads');
      if (!this.dependencies.isActive()) return;
      if (!result.idle) {
        const count = result.remainingOperationIds.length;
        this.state.cancelAllError = `Cancellation timed out with ${count} operation${count === 1 ? '' : 's'} still stopping. New work remains paused.`;
      }
    } catch (error) {
      if (!this.dependencies.isActive()) return;
      this.state.cancelAllError = `Cancel all did not drain cleanly: ${normalizeAppError(error)}`;
    }
  }

  async retryItem(item: QueueItem): Promise<void> {
    if (!canRetryItem(item)) return;
    this.dependencies.clearProgressDisplayState(item.id);
    await this.enqueueItems([item.id], true);
  }

  async removeSelected(): Promise<void> {
    const removableIds = this.dependencies
      .getItems()
      .filter((item) => item.selected && !isActiveStatus(item.status))
      .map((item) => item.id);
    if (removableIds.length === 0) return;
    this.state.queueActionError = null;
    try {
      await this.dependencies.invoke('remove_queue_items', { itemIds: removableIds });
      if (!this.dependencies.isActive()) return;
      const editingTitleId = this.dependencies.getEditingTitleId();
      if (editingTitleId && removableIds.includes(editingTitleId)) {
        this.dependencies.cancelFilenameEdit();
      }
    } catch (error) {
      if (!this.dependencies.isActive()) return;
      this.state.queueActionError = normalizeAppError(error);
    }
  }

  async clearCompleted(): Promise<void> {
    const itemIds = this.dependencies
      .getItems()
      .filter((item) => item.status === 'completed' || item.status === 'cancelled')
      .map((item) => item.id);
    if (itemIds.length === 0) return;
    try {
      await this.dependencies.invoke('remove_queue_items', { itemIds });
    } catch (error) {
      if (!this.dependencies.isActive()) return;
      this.state.queueActionError = normalizeAppError(error);
    }
  }

  async updateQueueItemSettings(
    item: QueueItem,
    input: {
      format?: OutputFormat;
      quality?: string;
      outputDir?: string;
      filenameOverride?: string | null;
    }
  ): Promise<void> {
    this.state.queueActionError = null;
    try {
      await this.dependencies.invoke('update_queue_item', { itemId: item.id, input });
    } catch (error) {
      if (!this.dependencies.isActive()) return;
      this.state.queueActionError = normalizeAppError(error);
      await this.dependencies.reloadAppSnapshot().catch(() => undefined);
    }
  }

  async applyGlobalQuality(): Promise<void> {
    await Promise.all(
      this.dependencies
        .getItems()
        .filter((item) => isEditablePendingStatus(item.status))
        .map((item) =>
          this.updateQueueItemSettings(item, {
            quality: resolveAvailableQuality(
              this.dependencies.getGlobalQuality(),
              item.availableQualities
            )
          })
        )
    );
  }

  async applyGlobalFormat(): Promise<void> {
    await Promise.all(
      this.dependencies
        .getItems()
        .filter((item) => isEditablePendingStatus(item.status))
        .map((item) =>
          this.updateQueueItemSettings(item, {
            format: resolveAvailableFormat(
              this.dependencies.getGlobalFormat(),
              item.hasAudio,
              'mp4'
            )
          })
        )
    );
  }
}
