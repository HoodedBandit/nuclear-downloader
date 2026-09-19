import { AppStateController } from './app-state-controller';
import { isTerminalOperation } from './backend-state';
import type { AppSnapshot } from './bindings/AppSnapshot';
import type { StateDelta } from './bindings/StateDelta';
import type { invokeCommand, listenEvent, EventMap, NuclearEvent } from './ipc-client';
import { OperationWaitRegistry } from './operation-wait-registry';
import { PageLifetime } from './page-lifetime';
import type { QueuePresentationController } from './queue-presentation';
import type { RuntimeWorkflowController, RuntimeWorkflowState } from './runtime-workflow';
import type { AppUpdateWorkflowController, AppUpdateWorkflowState } from './app-update-workflow';
import {
  createStartupSubsystems,
  type StartupSubsystem,
  type StartupSubsystemState
} from './startup-state';
import type { ErrorAttempt, UiErrorReporter } from './ui-error-reporter';

export function createAppSessionState() {
  return {
    subsystems: createStartupSubsystems(),
    criticalSubscribed: false,
    synchronized: false,
    optionalListenerFailures: 0,
    backendStateError: null as string | null,
    persistenceHealthError: null as string | null
  };
}
export type AppSessionState = ReturnType<typeof createAppSessionState>;
interface AppSessionOptions {
  lifetime: PageLifetime;
  invoke: typeof invokeCommand;
  listen: typeof listenEvent;
  errors: UiErrorReporter;
  queue: QueuePresentationController;
  runtime: RuntimeWorkflowState;
  appUpdate: AppUpdateWorkflowState;
}
interface SessionWorkflows {
  runtime: RuntimeWorkflowController;
  appUpdate: AppUpdateWorkflowController;
  initializeOutputDirectory: () => Promise<void>;
}

export class AppSessionController {
  readonly unloadedError = new Error('Renderer was unloaded before the operation completed.');
  private readonly lifetime: PageLifetime;
  private readonly waiters = new OperationWaitRegistry();
  private readonly snapshots: AppStateController;
  private connectionFailure: ErrorAttempt | null = null;
  private persistenceFailure: ErrorAttempt | null = null;

  constructor(
    readonly state: AppSessionState,
    private readonly options: AppSessionOptions
  ) {
    this.lifetime = options.lifetime;
    this.snapshots = new AppStateController(
      () => options.invoke('get_app_snapshot'),
      this.lifetime.guard((snapshot, delta) => this.applySnapshot(snapshot, delta)),
      this.lifetime.guard((error) => this.connectionError(error))
    );
    this.own(() => this.snapshots.stop());
    this.own(() => this.waiters.dispose(this.unloadedError));
    this.own(() => options.queue.dispose());
  }

  get isActive(): boolean {
    return this.lifetime.isActive;
  }
  own(dispose: () => void): void {
    this.lifetime.own(dispose);
  }
  dispose(): void {
    this.lifetime.dispose();
  }

  setSubsystem(subsystem: StartupSubsystem, state: StartupSubsystemState): void {
    if (this.isActive) this.state.subsystems[subsystem] = state;
  }

  reportStartupIssue(subsystem: string, error: unknown): void {
    if (this.isActive) this.options.errors.begin(`startup:${subsystem}`, subsystem).fail(error);
  }

  async start(workflows: SessionWorkflows): Promise<void> {
    try {
      await this.snapshots.start(
        (handler) =>
          this.options.listen(
            'app-state-changed',
            this.lifetime.guard((event) => handler(event.payload))
          ),
        (handler) =>
          this.options.listen(
            'app-state-resync-required',
            this.lifetime.guard((event) => handler(event.payload))
          )
      );
      if (!this.isActive) return;
      this.state.criticalSubscribed = true;
    } catch (error) {
      if (!this.isActive) return;
      this.state.criticalSubscribed = this.snapshots.subscriptionsActive;
      this.connectionError(error);
    }
    if (!this.isActive) return;
    await this.installOptional('download-progress', (payload) =>
      this.options.queue.applyProgress(payload)
    );
    if (!this.isActive) return;
    await this.installOptional('update-install-progress', (payload) =>
      workflows.appUpdate.applyProgress(payload)
    );
    if (!this.isActive) return;
    await this.installOptional('downloader-runtime-update-progress', (payload) =>
      workflows.runtime.applyProgress(payload)
    );
    if (!this.isActive) return;
    this.updateListenerReadiness();
    await Promise.all([
      workflows.appUpdate.initializeAppVersion(),
      workflows.runtime.initialize(),
      workflows.initializeOutputDirectory(),
      workflows.appUpdate.initializeUpdateCheck()
    ]);
  }

  async reload(): Promise<void> {
    try {
      await this.snapshots.reload();
    } catch (error) {
      if (this.isActive) this.connectionError(error);
      throw error;
    }
  }

  waitForOperation(id: string, timeoutMs = 35 * 60 * 1000) {
    return this.waiters.waitWithRefresh(
      id,
      () => this.options.queue.state.backendSnapshot?.operations ?? [],
      timeoutMs,
      () => this.reload()
    );
  }

  private async installOptional<K extends NuclearEvent>(
    event: K,
    handler: (payload: EventMap[K]) => void
  ): Promise<void> {
    try {
      this.own(
        await this.options.listen(
          event,
          this.lifetime.guard((message) => handler(message.payload))
        )
      );
    } catch (error) {
      if (!this.isActive) return;
      this.state.optionalListenerFailures += 1;
      this.reportStartupIssue(event, error);
    }
  }

  private connectionError(error: unknown): void {
    this.connectionFailure ??= this.options.errors.begin(
      'backend',
      'Connecting to the download engine'
    );
    this.state.backendStateError = this.connectionFailure.fail(error);
    this.state.synchronized = false;
    this.updateListenerReadiness();
    this.waiters.rejectAll(
      new Error(`The app state stream failed: ${this.state.backendStateError}`)
    );
  }

  private applySnapshot(snapshot: AppSnapshot, delta?: StateDelta): void {
    this.options.queue.applySnapshot(snapshot, delta);
    this.state.backendStateError = null;
    this.state.synchronized = true;
    this.connectionFailure = null;
    this.options.errors.resolve('backend');
    this.updateListenerReadiness();
    if (snapshot.persistenceHealth.degraded) {
      this.persistenceFailure ??= this.options.errors.begin('persistence', 'Saving queue history');
      this.state.persistenceHealthError = this.persistenceFailure.fail(
        snapshot.persistenceHealth.error ?? 'Queue history is not being saved.'
      );
    } else {
      this.state.persistenceHealthError = null;
      this.persistenceFailure = null;
      this.options.errors.resolve('persistence');
    }
    this.waiters.settle(snapshot.operations);
    this.options.runtime.updateRunning = snapshot.operations.some(
      (op) => op.kind === 'runtime_update' && !isTerminalOperation(op)
    );
    this.options.appUpdate.installRunning = snapshot.operations.some(
      (op) => op.kind === 'app_update' && !isTerminalOperation(op)
    );
  }

  private updateListenerReadiness(): void {
    this.setSubsystem(
      'listeners',
      !this.state.criticalSubscribed || !this.state.synchronized
        ? 'error'
        : this.state.optionalListenerFailures
          ? 'degraded'
          : 'ready'
    );
  }
}
