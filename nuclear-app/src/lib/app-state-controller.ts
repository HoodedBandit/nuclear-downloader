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

interface ControllerSession {
  generation: number;
  reconciler: StateReconciler<AppSnapshot, StateDelta>;
  reloadPromise: Promise<void> | null;
  requiredSequence: number;
  registrationsComplete: boolean;
  reloadRequested: boolean;
  unlisten: (() => void)[];
}

export class AppStateController {
  private generation = 0;
  private session: ControllerSession | null = null;

  constructor(
    private readonly loadSnapshot: () => Promise<AppSnapshot>,
    private readonly onSnapshot: (snapshot: AppSnapshot, delta?: StateDelta) => void,
    private readonly onError: (error: unknown) => void = () => undefined
  ) {}

  async start(
    subscribe: StateSubscription,
    subscribeResync: StateResyncSubscription
  ): Promise<void> {
    this.stop();
    const session: ControllerSession = {
      generation: ++this.generation,
      reconciler: new StateReconciler<AppSnapshot, StateDelta>(applyStateDelta),
      reloadPromise: null,
      requiredSequence: 0,
      registrationsComplete: false,
      reloadRequested: false,
      unlisten: []
    };
    this.session = session;
    try {
      if (
        !(await this.install(
          session,
          subscribe((delta) => this.handleDelta(session, delta))
        ))
      )
        return;
      if (
        !(await this.install(
          session,
          subscribeResync((request) => this.handleResync(session, request))
        ))
      )
        return;
      session.registrationsComplete = true;
      if (session.reloadRequested) session.reconciler.beginReload();
      await this.load(session, false);
    } catch (error) {
      if (!this.isCurrent(session)) return;
      this.end(session);
      throw error;
    }
  }

  stop(): void {
    this.generation += 1;
    const session = this.session;
    this.session = null;
    if (session) this.dispose(session);
  }

  async accept(unchecked: StateDelta): Promise<void> {
    const session = this.session;
    if (session) await this.acceptFor(session, unchecked);
  }

  async acceptResync(unchecked: AppStateResyncRequired): Promise<void> {
    const session = this.session;
    if (session) await this.acceptResyncFor(session, unchecked);
  }

  reload(): Promise<void> {
    const session = this.session;
    if (!session) return Promise.resolve();
    if (!session.registrationsComplete) {
      session.reloadRequested = true;
      return Promise.resolve();
    }
    return this.load(session, true);
  }

  current(): AppSnapshot | null {
    return this.session?.reconciler.current()?.value ?? null;
  }

  private async install(
    session: ControllerSession,
    pending: Promise<() => void>
  ): Promise<boolean> {
    const unlisten = await pending;
    if (!this.isCurrent(session)) {
      unlisten();
      return false;
    }
    session.unlisten.push(unlisten);
    return true;
  }

  private handleDelta(session: ControllerSession, delta: StateDelta): void {
    if (!this.isCurrent(session)) return;
    void this.acceptFor(session, delta).catch((error) => {
      if (this.isCurrent(session)) this.onError(error);
    });
  }

  private handleResync(session: ControllerSession, request: AppStateResyncRequired): void {
    if (!this.isCurrent(session)) return;
    void this.acceptResyncFor(session, request).catch((error) => {
      if (this.isCurrent(session)) this.onError(error);
    });
  }

  private async acceptFor(session: ControllerSession, unchecked: StateDelta): Promise<void> {
    if (!this.isCurrent(session)) return;
    const delta = validateStateDelta(unchecked);
    if (!this.isCurrent(session)) return;
    const applied = session.reconciler.push({ sequence: delta.sequence, value: delta });
    if (session.registrationsComplete && session.reconciler.needsRefetch()) {
      await this.load(session, true);
      return;
    }
    if (applied) this.publish(session, delta);
  }

  private async acceptResyncFor(
    session: ControllerSession,
    unchecked: AppStateResyncRequired
  ): Promise<void> {
    if (!this.isCurrent(session)) return;
    const latestSequence = unchecked?.latestSequence;
    if (!Number.isSafeInteger(latestSequence) || latestSequence < 0)
      throw new Error('Invalid app state resync sequence.');
    session.requiredSequence = Math.max(session.requiredSequence, latestSequence);
    if (session.registrationsComplete) await this.load(session, true);
  }

  private load(session: ControllerSession, beginReload: boolean): Promise<void> {
    if (!this.isCurrent(session)) return Promise.resolve();
    if (session.reloadPromise) return session.reloadPromise;
    if (beginReload) session.reconciler.beginReload();
    const reload = Promise.resolve().then(async () => {
      for (let attempt = 0; attempt < MAX_RECONCILIATION_ATTEMPTS; attempt += 1) {
        if (!this.isCurrent(session)) return;
        const unchecked = await this.loadSnapshot();
        if (!this.isCurrent(session)) return;
        const snapshot = validateAppSnapshot(unchecked);
        if (!this.isCurrent(session)) return;
        session.reconciler.load({ sequence: snapshot.latestSequence, value: snapshot });
        const current = session.reconciler.current();
        if (
          !session.reconciler.needsRefetch() &&
          current !== null &&
          current.sequence >= session.requiredSequence
        ) {
          this.publish(session);
          return;
        }
        session.reconciler.beginReload();
      }
      if (this.isCurrent(session))
        throw new Error('App state could not be reconciled after repeated sequence gaps.');
    });
    const ownedPromise = reload.finally(() => {
      if (this.isCurrent(session) && session.reloadPromise === ownedPromise)
        session.reloadPromise = null;
    });
    session.reloadPromise = ownedPromise;
    return ownedPromise;
  }

  private publish(session: ControllerSession, delta?: StateDelta): void {
    if (!this.isCurrent(session)) return;
    const current = session.reconciler.current();
    if (current) this.onSnapshot(current.value, delta);
  }

  private isCurrent(session: ControllerSession): boolean {
    return this.session === session && this.generation === session.generation;
  }

  private end(session: ControllerSession): void {
    if (!this.isCurrent(session)) return;
    this.generation += 1;
    this.session = null;
    this.dispose(session);
  }

  private dispose(session: ControllerSession): void {
    for (const unlisten of session.unlisten.splice(0)) {
      try {
        unlisten();
      } catch (error) {
        try {
          this.onError(error);
        } catch {
          // Continue releasing the remaining listeners.
        }
      }
    }
  }
}
