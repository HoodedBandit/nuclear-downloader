import { beforeEach, describe, expect, it, vi } from 'vitest';

const { invokeMock, listenMock } = vi.hoisted(() => ({
  invokeMock: vi.fn(),
  listenMock: vi.fn()
}));

vi.mock('@tauri-apps/api/core', () => ({ invoke: invokeMock }));
vi.mock('@tauri-apps/api/event', () => ({ listen: listenMock }));

describe('typed IPC client', () => {
  beforeEach(() => {
    invokeMock.mockReset();
    listenMock.mockReset();
  });

  it('uses backend-owned operation IDs for cancellation', async () => {
    invokeMock.mockResolvedValue(undefined);
    const { invokeCommand } = await import('./ipc-client');
    await invokeCommand('cancel_operation', { operationId: 'operation-id' });
    expect(invokeMock).toHaveBeenCalledWith('cancel_operation', {
      operationId: 'operation-id'
    });
  });

  it('registers the state channel before callers request a snapshot', async () => {
    const unlisten = vi.fn();
    listenMock.mockResolvedValue(unlisten);
    const { listenEvent } = await import('./ipc-client');
    const handler = vi.fn();
    await expect(listenEvent('app-state-changed', handler)).resolves.toBe(unlisten);
    expect(listenMock).toHaveBeenCalledWith('app-state-changed', handler);
  });
});
