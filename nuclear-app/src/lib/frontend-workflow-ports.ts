import type { OperationSnapshot } from './bindings/OperationSnapshot';
import type { invokeCommand } from './ipc-client';

export interface WorkflowLifetime {
  isActive: () => boolean;
  unloadedError: Error;
}

export interface WorkflowCommands extends WorkflowLifetime {
  invoke: typeof invokeCommand;
}

export interface OperationWorkflow extends WorkflowCommands {
  waitForOperation: (operationId: string, timeoutMs?: number) => Promise<OperationSnapshot>;
}
