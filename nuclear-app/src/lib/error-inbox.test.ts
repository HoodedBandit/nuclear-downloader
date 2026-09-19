import { describe, expect, it } from 'vitest';
import { createErrorInbox, syncErrors } from './error-inbox';

describe('Settings error history', () => {
  const source = {
    key: 'download-1',
    context: 'My video',
    detail: 'Download failed: diagnostic detail'
  };
  it('notifies once, clears on opening Settings, and preserves details after resolution', () => {
    const inbox = createErrorInbox();
    syncErrors(inbox, [source], false, 100);
    syncErrors(inbox, [source], false, 101);
    expect(inbox.entries).toHaveLength(1);
    expect(inbox.entries[0].read).toBe(false);
    syncErrors(inbox, [source], true, 102);
    expect(inbox.entries[0].read).toBe(true);
    syncErrors(inbox, [], false, 103);
    expect(inbox.entries[0].detail).toBe(source.detail);
    syncErrors(inbox, [source], false, 104);
    expect(inbox.entries).toHaveLength(2);
    expect(inbox.entries[0]).toMatchObject({ read: false, occurredAt: 104 });
  });
  it('marks errors received while Settings is open as seen, but not later failures', () => {
    const inbox = createErrorInbox();
    syncErrors(inbox, [source], true);
    expect(inbox.entries[0].read).toBe(true);
    syncErrors(inbox, [source, { ...source, key: 'download-2' }], false);
    expect(inbox.entries.map((entry) => entry.read)).toEqual([false, true]);
  });
  it('bounds history and retains the latest errors first', () => {
    const inbox = createErrorInbox();
    syncErrors(
      inbox,
      Array.from({ length: 210 }, (_, i) => ({ ...source, key: String(i), context: String(i) })),
      false
    );
    expect(inbox.entries).toHaveLength(200);
    expect(inbox.entries[0].context).toBe('209');
  });
});
