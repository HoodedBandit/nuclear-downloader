import { describe, expect, it } from 'vitest';
import { StateReconciler } from './state-reconciler';

describe('sequenced state reconciliation', () => {
  it('subscribes first by buffering deltas newer than the snapshot', () => {
    const reconciler = new StateReconciler<number, number>((state, delta) => state + delta);
    reconciler.push({ sequence: 11, value: 3 });
    reconciler.push({ sequence: 9, value: 100 });
    reconciler.load({ sequence: 10, value: 7 });

    expect(reconciler.current()).toEqual({ sequence: 11, value: 10 });
    expect(reconciler.needsRefetch()).toBe(false);
  });

  it('fails closed and requests a refetch on a sequence gap', () => {
    const reconciler = new StateReconciler<number, number>((state, delta) => state + delta);
    reconciler.load({ sequence: 20, value: 1 });
    reconciler.push({ sequence: 22, value: 5 });

    expect(reconciler.current()).toEqual({ sequence: 20, value: 1 });
    expect(reconciler.needsRefetch()).toBe(true);
  });

  it('buffers events during a gap-triggered snapshot refetch', () => {
    const reconciler = new StateReconciler<number, number>((state, delta) => state + delta);
    reconciler.load({ sequence: 20, value: 1 });
    reconciler.push({ sequence: 22, value: 5 });
    reconciler.beginReload();
    reconciler.push({ sequence: 24, value: 7 });
    reconciler.load({ sequence: 23, value: 10 });

    expect(reconciler.current()).toEqual({ sequence: 24, value: 17 });
    expect(reconciler.needsRefetch()).toBe(false);
  });

  it('ignores stale or duplicate events', () => {
    const reconciler = new StateReconciler<number, number>((state, delta) => state + delta);
    reconciler.load({ sequence: 5, value: 10 });
    reconciler.push({ sequence: 5, value: 20 });
    reconciler.push({ sequence: 4, value: 20 });

    expect(reconciler.current()).toEqual({ sequence: 5, value: 10 });
  });
});
