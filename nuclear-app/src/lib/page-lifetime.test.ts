import { describe, expect, it, vi } from 'vitest';
import { PageLifetime } from './page-lifetime';

describe('PageLifetime', () => {
  it('immediately releases a listener that resolves after disposal', async () => {
    const lifetime = new PageLifetime(vi.fn());
    const early = vi.fn();
    const late = vi.fn();
    let resolveListener!: (disposer: () => void) => void;
    const listener = new Promise<() => void>((resolve) => {
      resolveListener = resolve;
    });
    lifetime.own(early);
    const registerListener = async (): Promise<void> => lifetime.own(await listener);
    const pendingRegistration = registerListener();

    lifetime.dispose();
    resolveListener(late);
    await pendingRegistration;

    expect(early).toHaveBeenCalledOnce();
    expect(late).toHaveBeenCalledOnce();
  });

  it('suppresses guarded callbacks after disposal', () => {
    const lifetime = new PageLifetime(vi.fn());
    const callback = vi.fn();
    const guarded = lifetime.guard(callback);
    guarded('before');
    lifetime.dispose();
    guarded('after');

    expect(callback).toHaveBeenCalledExactlyOnceWith('before');
  });

  it('disposes every owned resource exactly once', () => {
    const lifetime = new PageLifetime(vi.fn());
    const dispose = vi.fn();
    lifetime.own(dispose);
    lifetime.dispose();
    lifetime.dispose();

    expect(dispose).toHaveBeenCalledOnce();
  });

  it('reports a cleanup failure and still releases every remaining resource', () => {
    const reportError = vi.fn();
    const lifetime = new PageLifetime(reportError);
    const failure = new Error('listener cleanup failed');
    const first = vi.fn(() => {
      throw failure;
    });
    const second = vi.fn();
    lifetime.own(first);
    lifetime.own(second);

    lifetime.dispose();
    lifetime.dispose();

    expect(first).toHaveBeenCalledOnce();
    expect(second).toHaveBeenCalledOnce();
    expect(reportError).toHaveBeenCalledExactlyOnceWith(failure);
  });
});
