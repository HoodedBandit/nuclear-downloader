import type { AppSnapshot } from './bindings/AppSnapshot';
import type { AppStateResyncRequired } from './bindings/AppStateResyncRequired';
import type { StateDelta } from './bindings/StateDelta';
import { applyStateDelta, validateAppSnapshot, validateStateDelta } from './backend-state';
import { StateReconciler } from './state-reconciler';

const MAX_RECONCILIATION_ATTEMPTS = 3;

export type StateSubscription = (handler: (delta: StateDelta) => void) => Promise<() => void>;
export type StateResyncSubscription = (
  handler: (request: AppStateResyncRequired) => void
) => Promise<() => void>;

export class AppStateController {
  private readonly reconciler = new StateReconciler<AppSnapshot, StateDelta>(applyStateDelta);
  private reloadPromise: Promise<void> | null = null;
  private requiredSequence = 0;
  private unlisten: (() => void)[] = [];

  constructor(
    private readonly loadSnapshot: () => Promise<AppSnapshot>,
    private readonly onSnapshot: (snapshot: AppSnapshot, delta?: StateDelta) => void,
    private readonly onError: (error: unknown) => void = () => undefined
  ) {}

  async start(
    subscribe: StateSubscription,
    subscribeResync: StateResyncSubscription
  ): Promise<void> {
    // Subscribe first so no backend change can land between snapshot and listener setup.
    try {
      this.unlisten.push(
        await subscribe((delta) => {
          void this.accept(delta).catch(this.onError);
        })
      );
      this.unlisten.push(
        await subscribeResync((request) => {
          void this.acceptResync(request).catch(this.onError);
        })
      );
      await this.load(false);
    } catch (error) {
      this.stop();
      throw error;
    }
  }

  stop(): void {
    for (const unlisten of this.unlisten.splice(0)) unlisten();
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

  async acceptResync(unchecked: AppStateResyncRequired): Promise<void> {
    const latestSequence = unchecked?.latestSequence;
    if (!Number.isSafeInteger(latestSequence) || latestSequence < 0) {
      throw new Error('Invalid app state resync sequence.');
    }
    this.requiredSequence = Math.max(this.requiredSequence, latestSequence);
    await this.reload();
  }

  reload(): Promise<void> {
    return this.load(true);
  }

  current(): AppSnapshot | null {
    return this.reconciler.current()?.value ?? null;
  }

  private load(beginReload: boolean): Promise<void> {
    if (beginReload) this.reconciler.beginReload();
    if (this.reloadPromise) return this.reloadPromise;
    const reload = Promise.resolve().then(async () => {
      for (let attempt = 0; attempt < MAX_RECONCILIATION_ATTEMPTS; attempt += 1) {
        const snapshot = validateAppSnapshot(await this.loadSnapshot());
        this.reconciler.load({ sequence: snapshot.latestSequence, value: snapshot });
        const current = this.reconciler.current();
        if (
          !this.reconciler.needsRefetch() &&
          current !== null &&
          current.sequence >= this.requiredSequence
        ) {
          this.publish();
          return;
        }

        // A second gap can be observed while this snapshot is in flight. Start
        // another buffered load immediately instead of waiting indefinitely for
        // an unrelated future event to trigger recovery.
        this.reconciler.beginReload();
      }

      throw new Error('App state could not be reconciled after repeated sequence gaps.');
    });
    this.reloadPromise = reload.finally(() => {
      this.reloadPromise = null;
    });
    return this.reloadPromise;
  }

  private publish(delta?: StateDelta): void {
    const current = this.reconciler.current();
    if (current) this.onSnapshot(current.value, delta);
  }
}
