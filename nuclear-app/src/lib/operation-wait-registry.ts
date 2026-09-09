import type { OperationSnapshot } from '$lib/bindings/OperationSnapshot';

interface PendingOperationWait {
  resolve: (operation: OperationSnapshot) => void;
  reject: (error: Error) => void;
  timeout?: ReturnType<typeof setTimeout>;
  refreshTimeout?: ReturnType<typeof setTimeout>;
}

export class OperationWaitRegistry {
  private readonly pending = new Map<string, PendingOperationWait>();
  private disposedError: Error | undefined;

  wait(
    operationId: string,
    operations: readonly OperationSnapshot[],
    timeoutMs: number
  ): Promise<OperationSnapshot> {
    return this.register(operationId, operations, timeoutMs).promise;
  }

  private register(
    operationId: string,
    operations: readonly OperationSnapshot[],
    timeoutMs: number
  ): { promise: Promise<OperationSnapshot>; waiter?: PendingOperationWait } {
    if (this.disposedError) return { promise: Promise.reject(this.disposedError) };
    const existing = operations.find((operation) => operation.id === operationId);
    if (existing && isTerminalOperation(existing)) {
      const previous = this.pending.get(operationId);
      if (previous && this.release(operationId, previous)) previous.resolve(existing);
      return { promise: Promise.resolve(existing) };
    }

    let registered: PendingOperationWait | undefined;
    const promise = new Promise<OperationSnapshot>((resolve, reject) => {
      const previous = this.pending.get(operationId);
      if (previous) {
        this.reject(
          operationId,
          previous,
          new Error(`Operation ${operationId} was already being awaited.`)
        );
      }

      const waiter: PendingOperationWait = { resolve, reject };
      this.pending.set(operationId, waiter);
      registered = waiter;
      waiter.timeout = setTimeout(() => {
        this.reject(
          operationId,
          waiter,
          new Error(`Timed out waiting for operation ${operationId} to reach a terminal state.`)
        );
      }, timeoutMs);
    });
    return { promise, waiter: registered };
  }

  /**
   * Waits on the event stream while periodically reconciling with the
   * authoritative backend snapshot. Tauri events are notifications, not a
   * durable queue: a renderer reload or a single delivery failure must not
   * leave a completed operation looking permanently stuck.
   */
  async waitWithRefresh(
    operationId: string,
    readOperations: () => readonly OperationSnapshot[],
    timeoutMs: number,
    refresh: () => Promise<void>,
    refreshIntervalMs = 1_000
  ): Promise<OperationSnapshot> {
    if (this.disposedError) throw this.disposedError;
    const { promise, waiter } = this.register(operationId, readOperations(), timeoutMs);
    if (waiter) {
      this.scheduleRefresh(operationId, waiter, readOperations, refresh, refreshIntervalMs);
    }
    return promise;
  }

  private scheduleRefresh(
    operationId: string,
    waiter: PendingOperationWait,
    readOperations: () => readonly OperationSnapshot[],
    refresh: () => Promise<void>,
    intervalMs: number
  ): void {
    waiter.refreshTimeout = setTimeout(() => {
      waiter.refreshTimeout = undefined;
      const reconcile = async (): Promise<void> => {
        if (this.pending.get(operationId) !== waiter) return;
        await refresh();
        // An in-flight refresh belongs only to the waiter that started it.
        // Replacement or disposal cannot transfer its result to a new wait.
        if (this.pending.get(operationId) !== waiter) return;
        this.settle(readOperations());
        if (this.pending.get(operationId) === waiter) {
          this.scheduleRefresh(operationId, waiter, readOperations, refresh, intervalMs);
        }
      };
      void reconcile().catch((error: unknown) => this.reject(operationId, waiter, toError(error)));
    }, intervalMs);
  }

  settle(operations: readonly OperationSnapshot[]): void {
    for (const [operationId, waiter] of this.pending) {
      const operation = operations.find((candidate) => candidate.id === operationId);
      if (!operation || !isTerminalOperation(operation)) continue;

      if (this.release(operationId, waiter)) waiter.resolve(operation);
    }
  }

  rejectAll(error: Error): void {
    for (const [operationId, waiter] of this.pending) {
      this.reject(operationId, waiter, error);
    }
  }

  /** Closes renderer-owned waits without cancelling their backend operations. */
  dispose(error: Error): void {
    if (this.disposedError) return;
    this.disposedError = error;
    this.rejectAll(error);
  }

  private release(operationId: string, waiter: PendingOperationWait): boolean {
    if (this.pending.get(operationId) !== waiter) return false;
    this.pending.delete(operationId);
    clearTimeout(waiter.timeout);
    clearTimeout(waiter.refreshTimeout);
    return true;
  }

  private reject(operationId: string, waiter: PendingOperationWait, error: Error): void {
    if (this.release(operationId, waiter)) waiter.reject(error);
  }

  get size(): number {
    return this.pending.size;
  }
}

function toError(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error));
}

function isTerminalOperation(operation: OperationSnapshot): boolean {
  return (
    operation.state === 'completed' ||
    operation.state === 'failed' ||
    operation.state === 'cancelled' ||
    operation.state === 'interrupted'
  );
}
