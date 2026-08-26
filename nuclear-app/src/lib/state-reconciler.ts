export interface SequencedSnapshot<T> {
  sequence: number;
  value: T;
}

export interface SequencedDelta<T> {
  sequence: number;
  value: T;
}

export type DeltaApplication<TState, TDelta> = (state: TState, delta: TDelta) => TState;

/**
 * Buffers deltas while the initial snapshot is loading and rejects sequence
 * gaps. The caller owns the refetch policy so transport failures remain
 * visible instead of being hidden inside this pure state machine.
 */
export class StateReconciler<TState, TDelta> {
  private state: TState | null = null;
  private sequence = 0;
  private loading = true;
  private gap = false;
  private readonly buffered: SequencedDelta<TDelta>[] = [];

  constructor(private readonly applyDelta: DeltaApplication<TState, TDelta>) {}

  push(delta: SequencedDelta<TDelta>): void {
    if (this.loading) {
      this.buffered.push(delta);
      return;
    }

    this.apply(delta);
  }

  beginReload(): void {
    this.loading = true;
    this.buffered.length = 0;
  }

  load(snapshot: SequencedSnapshot<TState>): void {
    this.state = snapshot.value;
    this.sequence = snapshot.sequence;
    this.gap = false;
    this.loading = false;

    this.buffered
      .sort((left, right) => left.sequence - right.sequence)
      .forEach((delta) => this.apply(delta));
    this.buffered.length = 0;
  }

  needsRefetch(): boolean {
    return this.gap;
  }

  current(): SequencedSnapshot<TState> | null {
    if (this.state === null) return null;
    return { sequence: this.sequence, value: this.state };
  }

  private apply(delta: SequencedDelta<TDelta>): void {
    if (this.gap || this.state === null || delta.sequence <= this.sequence) return;
    if (delta.sequence !== this.sequence + 1) {
      this.gap = true;
      return;
    }

    this.state = this.applyDelta(this.state, delta.value);
    this.sequence = delta.sequence;
  }
}
