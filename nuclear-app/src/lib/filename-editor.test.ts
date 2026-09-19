import { describe, expect, it, vi } from 'vitest';
import { FilenameEditorController, createFilenameEditorState } from './filename-editor';
import { projectQueueItem, sanitizeFilenameDraft } from './queue-presentation-helpers';
import type { QueueItemRecord } from './bindings/QueueItemRecord';
import { createErrorInbox } from './error-inbox';
import { InterfaceErrorReporter } from './ui-error-reporter';

function setup() {
  const records = ['one', 'two'].map(
    (id) =>
      ({
        schemaVersion: 1,
        id,
        sourceUrl: `https://example.test/${id}`,
        title: id,
        availableQualities: ['best'],
        hasAudio: true,
        cookieConfig: null,
        format: 'mp4',
        quality: 'best',
        outputDir: 'C:\\Downloads',
        filenameOverride: null,
        compatConfigPath: null,
        state: 'inert',
        latestOperationId: null,
        createdAtMs: 1,
        updatedAtMs: 1
      }) satisfies QueueItemRecord
  );
  const items = records.map((record) => projectQueueItem(record, null));
  const state = createFilenameEditorState();
  const inbox = createErrorInbox();
  let active = true;
  const save = vi.fn<(id: string, filename: string | null) => Promise<void>>(async () => {});
  const editor = new FilenameEditorController(state, {
    getItems: () => items,
    save,
    isActive: () => active,
    errors: new InterfaceErrorReporter(
      inbox,
      () => false,
      () => active
    )
  });
  return {
    editor,
    state,
    items,
    save,
    inbox,
    dispose: () => {
      active = false;
    }
  };
}

describe('filename editing ownership', () => {
  it.each(['ready', 'queued'] as const)(
    'renames a %s item through the existing filename command',
    async (status) => {
      const { editor, items, state, save } = setup();
      items[0].status = status;
      await editor.begin(items[0]);
      editor.setDraft('CON.txt.MP4');
      expect(await editor.commit()).toBe(true);
      expect(save).toHaveBeenCalledWith('one', 'CON_.txt');
      expect(state.itemId).toBeNull();
      expect(sanitizeFilenameDraft('   ')).toBe('');
    }
  );

  it.each(['downloading', 'postprocessing', 'cancelling', 'completed'] as const)(
    'does not edit a %s file',
    async (status) => {
      const { editor, items, state, save } = setup();
      items[0].status = status;
      await editor.begin(items[0]);
      expect(state.itemId).toBeNull();
      expect(save).not.toHaveBeenCalled();
    }
  );

  it('does not edit metadata that is still being prepared', async () => {
    const { editor, items, state } = setup();
    items[0].infoLoaded = false;
    await editor.begin(items[0]);
    expect(state.itemId).toBeNull();
  });

  it('retains invalid and rejected drafts and records each failed attempt', async () => {
    const { editor, items, state, save, inbox } = setup();
    await editor.begin(items[0]);
    editor.setDraft('  ');
    expect(await editor.commit()).toBe(false);
    expect(save).not.toHaveBeenCalled();
    expect(state.itemId).toBe('one');
    editor.setDraft('Keep my draft');
    save.mockRejectedValueOnce(new Error('worker claimed this file'));
    expect(await editor.commit()).toBe(false);
    expect(state.draft).toBe('Keep my draft');
    expect(inbox.entries).toHaveLength(2);
    expect(state.error).toContain('worker claimed');
  });

  it('deduplicates Enter and blur while a rename is saving', async () => {
    const { editor, items, save } = setup();
    let finish!: () => void;
    save.mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      })
    );
    await editor.begin(items[0]);
    editor.setDraft('New name');
    const enter = editor.commit();
    const blur = editor.commit();
    expect(blur).toBe(enter);
    expect(save).toHaveBeenCalledOnce();
    finish();
    expect(await enter).toBe(true);
  });

  it.each([false, true])('isolates a late save from another edit (reject=%s)', async (reject) => {
    const { editor, items, state, save, inbox } = setup();
    let finish!: () => void;
    save.mockReturnValueOnce(
      new Promise((resolve, fail) => {
        finish = () => (reject ? fail(new Error('old failure')) : resolve());
      })
    );
    await editor.begin(items[0]);
    editor.setDraft('First');
    const pending = editor.commit();
    editor.cancel();
    await editor.begin(items[1]);
    editor.setDraft('Second');
    finish();
    await pending;
    expect(state).toMatchObject({ itemId: 'two', draft: 'Second', error: '' });
    expect(inbox.entries).toHaveLength(0);
  });

  it('keeps a reopened same-row edit safe from the previous save', async () => {
    const { editor, items, state, save } = setup();
    let finish!: () => void;
    save.mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      })
    );
    await editor.begin(items[0]);
    const pending = editor.commit();
    editor.cancel();
    await editor.begin(items[0]);
    finish();
    await pending;
    expect(state.itemId).toBe('one');
  });

  it('flushes newer input before allowing its download to start', async () => {
    const { editor, items, state, save } = setup();
    let finish!: () => void;
    save.mockReturnValueOnce(
      new Promise((resolve) => {
        finish = resolve;
      })
    );
    await editor.begin(items[0]);
    editor.setDraft('First draft');
    const pending = editor.commit();
    editor.setDraft('Final draft');
    const flush = editor.flushFor(['one']);
    finish();
    await pending;
    expect(await flush).toBe(true);
    expect(save.mock.calls.map((call) => call[1])).toEqual(['First draft', 'Final draft']);
    expect(state.itemId).toBeNull();
  });

  it('does not publish a late failure after disposal', async () => {
    const { editor, items, state, save, dispose, inbox } = setup();
    let reject!: (error: Error) => void;
    save.mockReturnValueOnce(
      new Promise((_, fail) => {
        reject = fail;
      })
    );
    await editor.begin(items[0]);
    const pending = editor.commit();
    dispose();
    reject(new Error('late'));
    await pending;
    expect(state.error).toBe('');
    expect(inbox.entries).toHaveLength(0);
  });
});
