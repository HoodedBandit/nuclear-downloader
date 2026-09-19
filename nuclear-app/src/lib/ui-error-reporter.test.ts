import { describe, expect, it } from 'vitest';
import { createErrorInbox, markErrorsRead } from './error-inbox';
import { InterfaceErrorReporter } from './ui-error-reporter';

describe('synchronous UI error reporting', () => {
  it('records one failure per attempt and retains history when health recovers', () => {
    const inbox = createErrorInbox();
    const errors = new InterfaceErrorReporter(
      inbox,
      () => false,
      () => true
    );
    const first = errors.begin('runtime-update', 'Updating tools');
    first.fail('failure');
    first.fail('failure from the operation reply');
    expect(inbox.entries).toHaveLength(1);
    errors.resolve('runtime-update');
    expect(inbox.reportedActive).toEqual({});
    expect(inbox.entries).toHaveLength(1);
    errors.begin('runtime-update', 'Updating tools').fail('failure');
    expect(inbox.entries).toHaveLength(2);
  });
  it('marks Settings errors read immediately and ignores callbacks after disposal', () => {
    const inbox = createErrorInbox();
    let open = false;
    let active = true;
    const errors = new InterfaceErrorReporter(
      inbox,
      () => open,
      () => active
    );
    errors.begin('folder', 'Folder').fail('first');
    expect(inbox.entries[0].read).toBe(false);
    open = true;
    markErrorsRead(inbox);
    errors.begin('folder', 'Folder').fail('second');
    expect(inbox.entries.every((entry) => entry.read)).toBe(true);
    const pending = errors.begin('folder', 'Folder');
    active = false;
    pending.fail('late');
    expect(inbox.entries).toHaveLength(2);
  });
});
