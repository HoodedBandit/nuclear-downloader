import type { QueueItem } from './frontend-types';
import {
  getQueueItemDisplayTitle,
  isActiveStatus,
  QUEUE_ROW_HEIGHT_PX
} from './queue-presentation';

export type QueueFilter = 'all' | 'active' | 'queued' | 'completed' | 'attention';
export const filterLabels: Record<QueueFilter, string> = {
  all: 'Downloads',
  active: 'In progress',
  queued: 'Queued',
  completed: 'Completed',
  attention: 'Needs attention'
};
export function matchesFilter(item: QueueItem, filter: QueueFilter): boolean {
  switch (filter) {
    case 'active':
      return isActiveStatus(item.status) || item.status === 'fetching';
    case 'queued':
      return item.status === 'ready' || item.status === 'queued';
    case 'completed':
      return item.status === 'completed';
    case 'attention':
      return item.status === 'error' || item.status === 'cancelled';
    default:
      return true;
  }
}
export function filterQueue(
  items: readonly QueueItem[],
  filter: QueueFilter,
  search: string
): QueueItem[] {
  const query = search.trim().toLocaleLowerCase();
  return items.filter(
    (item) =>
      matchesFilter(item, filter) &&
      (!query ||
        [getQueueItemDisplayTitle(item), item.channel ?? '', item.url].some((value) =>
          value.toLocaleLowerCase().includes(query)
        ))
  );
}
export function queueViewWindow(items: readonly QueueItem[], scrollTop: number, height: number) {
  const start = Math.min(
    Math.max(0, items.length - 1),
    Math.max(0, Math.floor(scrollTop / QUEUE_ROW_HEIGHT_PX) - 8)
  );
  const end = Math.min(items.length, Math.ceil((scrollTop + height) / QUEUE_ROW_HEIGHT_PX) + 8);
  return {
    start,
    end,
    rows: items.slice(start, end).map((item, offset) => ({ item, index: start + offset })),
    topSpacerHeight: start * QUEUE_ROW_HEIGHT_PX,
    bottomSpacerHeight: Math.max(0, (items.length - end) * QUEUE_ROW_HEIGHT_PX)
  };
}
export function sourceLabel(url: string): string {
  try {
    const hostname = new URL(url).hostname.replace(/^www\./, '');
    if (hostname === 'youtu.be' || hostname.endsWith('youtube.com')) return 'YouTube';
    if (hostname === 'x.com' || hostname.endsWith('twitter.com')) return 'X';
    return hostname;
  } catch {
    return 'Media';
  }
}

export function countQueueFilters(items: readonly QueueItem[]): Record<QueueFilter, number> {
  return {
    all: items.length,
    active: items.filter((item) => matchesFilter(item, 'active')).length,
    queued: items.filter((item) => matchesFilter(item, 'queued')).length,
    completed: items.filter((item) => matchesFilter(item, 'completed')).length,
    attention: items.filter((item) => matchesFilter(item, 'attention')).length
  };
}
