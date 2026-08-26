import type { AppSnapshot } from './bindings/AppSnapshot';
import type { StateDelta } from './bindings/StateDelta';
import { applyStateDelta, validateAppSnapshot, validateStateDelta } from './backend-state';
import { StateReconciler } from './state-reconciler';

const MAX_RECONCILIATION_ATTEMPTS = 3;

export type StateSubscription = (handler: (delta: StateDelta) => void) => Promise<() => void>;

export class AppStateController {
  private readonly reconciler = new StateReconciler<AppSnapshot, StateDelta>(applyStateDelta);
  private reloadPromise: Promise<void> | null = null;
  private unlisten: (() => void) | null = null;

  constructor(
    private readonly loadSnapshot: () => Promise<AppSnapshot>,
    private readonly onSnapshot: (snapshot: AppSnapshot, delta?: StateDelta) => void,
    private readonly onError: (error: unknown) => void = () => undefined
  ) {}

  async start(subscribe: StateSubscription): Promise<void> {
    // Subscribe first so no backend change can land between snapshot and listener setup.
    this.unlisten = await subscribe((delta) => {
      void this.accept(delta).catch(this.onError);
    });
    try {
      await this.load(false);
    } catch (error) {
      this.stop();
      throw error;
    }
  }

  stop(): void {
    this.unlisten?.();
    this.unlisten = null;
  }

  async accept(unchecked: StateDelta): Promise<void> {
    const delta = validateStateDelta(unchecked);
    this.reconciler.push({ sequence: delta.sequence, value: delta });
    if (this.reconciler.needsRefetch()) {
      await this.reload();
      return;
    }
    this.publish(delta);
  }

  reload(): Promise<void> {
    return this.load(true);
  }

  current(): AppSnapshot | null {
    return this.reconciler.current()?.value ?? null;
  }

  private load(beginReload: boolean): Promise<void> {
    if (this.reloadPromise) return this.reloadPromise;
    if (beginReload) this.reconciler.beginReload();
    this.reloadPromise = (async () => {
      for (let attempt = 0; attempt < MAX_RECONCILIATION_ATTEMPTS; attempt += 1) {
        const snapshot = validateAppSnapshot(await this.loadSnapshot());
        this.reconciler.load({ sequence: snapshot.latestSequence, value: snapshot });
        if (!this.reconciler.needsRefetch()) {
          this.publish();
          return;
        }

        // A second gap can be observed while this snapshot is in flight. Start
        // another buffered load immediately instead of waiting indefinitely for
        // an unrelated future event to trigger recovery.
        this.reconciler.beginReload();
      }

      throw new Error('App state could not be reconciled after repeated sequence gaps.');
    })().finally(() => {
      this.reloadPromise = null;
    });
    return this.reloadPromise;
  }

  private publish(delta?: StateDelta): void {
    const current = this.reconciler.current();
    if (current) this.onSnapshot(current.value, delta);
  }
}
