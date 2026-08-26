import type { OperationSnapshot } from '$lib/bindings/OperationSnapshot';

interface PendingOperationWait {
  resolve: (operation: OperationSnapshot) => void;
  reject: (error: Error) => void;
  timeout: ReturnType<typeof setTimeout>;
}

export class OperationWaitRegistry {
  private readonly pending = new Map<string, PendingOperationWait>();

  wait(
    operationId: string,
    operations: readonly OperationSnapshot[],
    timeoutMs: number
  ): Promise<OperationSnapshot> {
    const existing = operations.find((operation) => operation.id === operationId);
    if (existing && isTerminalOperation(existing)) return Promise.resolve(existing);

    return new Promise((resolve, reject) => {
      const previous = this.pending.get(operationId);
      if (previous) {
        clearTimeout(previous.timeout);
        previous.reject(new Error(`Operation ${operationId} was already being awaited.`));
      }

      const timeout = setTimeout(() => {
        this.pending.delete(operationId);
        reject(
          new Error(`Timed out waiting for operation ${operationId} to reach a terminal state.`)
        );
      }, timeoutMs);
      this.pending.set(operationId, { resolve, reject, timeout });
      this.settle(operations);
    });
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
    const wait = this.wait(operationId, readOperations(), timeoutMs);
    let waiting = true;

    const reconcile = async (): Promise<void> => {
      while (waiting) {
        await delay(refreshIntervalMs);
        if (!waiting) return;

        await refresh();
        this.settle(readOperations());
      }
    };

    void reconcile().catch((error: unknown) => {
      this.reject(operationId, toError(error));
    });

    try {
      return await wait;
    } finally {
      waiting = false;
    }
  }

  settle(operations: readonly OperationSnapshot[]): void {
    for (const [operationId, waiter] of this.pending) {
      const operation = operations.find((candidate) => candidate.id === operationId);
      if (!operation || !isTerminalOperation(operation)) continue;

      this.pending.delete(operationId);
      clearTimeout(waiter.timeout);
      waiter.resolve(operation);
    }
  }

  rejectAll(error: Error): void {
    for (const waiter of this.pending.values()) {
      clearTimeout(waiter.timeout);
      waiter.reject(error);
    }
    this.pending.clear();
  }

  private reject(operationId: string, error: Error): void {
    const waiter = this.pending.get(operationId);
    if (!waiter) return;

    this.pending.delete(operationId);
    clearTimeout(waiter.timeout);
    waiter.reject(error);
  }

  get size(): number {
    return this.pending.size;
  }
}

function delay(milliseconds: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, milliseconds));
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
