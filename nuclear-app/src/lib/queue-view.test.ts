import { describe, expect, it } from 'vitest';
import type { QueueItem } from './frontend-types';
import { filterQueue, queueViewWindow } from './queue-view';
import { createQueueViewState, QueueViewController } from './queue-view-controller';

function items(count: number): QueueItem[] {
  return Array.from(
    { length: count },
    (_, index) =>
      ({
        id: String(index),
        title: `Item ${index}`,
        customFilename: null,
        selected: false,
        status: 'ready',
        url: `https://example.test/${index}`,
        channel: null
      }) as QueueItem
  );
}
describe('live queue view', () => {
  it('calculates exact virtual spacers and handles a queue shrink', () => {
    const queue = items(40);
    expect(queueViewWindow(queue, 880, 880)).toMatchObject({
      start: 2,
      end: 28,
      topSpacerHeight: 176,
      bottomSpacerHeight: 1056
    });
    expect(queueViewWindow(queue.slice(0, 5), 2640, 880)).toMatchObject({
      start: 4,
      end: 5,
      topSpacerHeight: 352,
      bottomSpacerHeight: 0
    });
  });
  it('selects only the filtered rows and resets selection and scroll when changing views', () => {
    const queue = items(12);
    const state = createQueueViewState();
    const controller = new QueueViewController(state, () => queue);
    state.search.query = 'Item 1';
    controller.selectVisible();
    expect(queue.filter((item) => item.selected).map((item) => item.id)).toEqual(['1', '10', '11']);
    expect(controller.selection()).toBe('all');
    controller.scroll(880);
    controller.changeFilter('completed');
    expect(controller.selectedCount()).toBe(0);
    expect(state.viewport.scrollTop).toBe(0);
  });
  it('searches custom filenames and applies the selected status filter', () => {
    const queue = items(2);
    queue[1].customFilename = 'Renamed clip';
    expect(filterQueue(queue, 'queued', 'renamed').map((item) => item.id)).toEqual(['1']);
    expect(filterQueue(queue, 'completed', 'renamed')).toHaveLength(0);
  });
});
