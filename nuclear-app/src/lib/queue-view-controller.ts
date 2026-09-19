import type { QueueItem } from './frontend-types';
import { countQueueFilters, filterQueue, queueViewWindow, type QueueFilter } from './queue-view';
import { deriveSelectionState } from './queue-logic';

export function createQueueViewState() {
  return {
    filter: 'all' as QueueFilter,
    search: { query: '', open: false },
    viewport: { scrollTop: 0, height: 600 }
  };
}
export type QueueViewState = ReturnType<typeof createQueueViewState>;

export class QueueViewController {
  private viewport: HTMLElement | null = null;
  private observer: ResizeObserver | null = null;
  constructor(
    readonly state: QueueViewState,
    private readonly getItems: () => readonly QueueItem[]
  ) {}

  visibleItems(): QueueItem[] {
    return filterQueue(this.getItems(), this.state.filter, this.state.search.query);
  }
  counts() {
    return countQueueFilters(this.getItems());
  }
  selection() {
    return deriveSelectionState(this.visibleItems());
  }
  selectedCount(): number {
    return this.getItems().filter((item) => item.selected).length;
  }
  window() {
    return queueViewWindow(
      this.visibleItems(),
      this.state.viewport.scrollTop,
      this.state.viewport.height
    );
  }

  attach(viewport: HTMLElement): () => void {
    this.viewport = viewport;
    this.state.viewport.height = viewport.clientHeight || this.state.viewport.height;
    if (typeof ResizeObserver !== 'undefined') {
      this.observer = new ResizeObserver(([entry]) => {
        if (this.viewport && entry) this.state.viewport.height = entry.contentRect.height;
      });
      this.observer.observe(viewport);
    }
    return () => {
      this.observer?.disconnect();
      this.observer = null;
      this.viewport = null;
    };
  }

  scroll(scrollTop: number): void {
    this.state.viewport.scrollTop = scrollTop;
  }
  resetScroll(): void {
    this.state.viewport.scrollTop = 0;
    if (this.viewport) this.viewport.scrollTop = 0;
  }
  clampScroll(): void {
    const max = this.viewport
      ? Math.max(0, this.viewport.scrollHeight - this.viewport.clientHeight)
      : 0;
    if (this.state.viewport.scrollTop > max) {
      this.state.viewport.scrollTop = max;
      if (this.viewport) this.viewport.scrollTop = max;
    }
  }
  changeFilter(filter: QueueFilter): void {
    this.state.filter = filter;
    for (const item of this.getItems()) item.selected = false;
    this.resetScroll();
  }
  setSelected(id: string, selected: boolean): void {
    const item = this.getItems().find((candidate) => candidate.id === id);
    if (item) item.selected = selected;
  }
  selectVisible(): void {
    const selected = this.selection() !== 'all';
    for (const item of this.visibleItems()) item.selected = selected;
  }
  async toggleSearch(focus: () => Promise<void>): Promise<void> {
    this.state.search.open = !this.state.search.open;
    if (this.state.search.open) await focus();
    else {
      this.state.search.query = '';
      this.resetScroll();
    }
  }
}
