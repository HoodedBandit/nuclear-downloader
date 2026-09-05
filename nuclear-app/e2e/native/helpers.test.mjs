import { afterEach, describe, expect, it, vi } from 'vitest';
import { addUrl, waitForTerminalQueueStatus } from './helpers.mjs';

afterEach(() => vi.unstubAllGlobals());

describe('rendered terminal status', () => {
  it.each([
    ['Completed', 'completed'],
    [' Cancelled ', 'cancelled']
  ])('accepts CSS-capitalized %s after an active phase', async (rendered, expected) => {
    const status = {
      getText: vi.fn().mockResolvedValueOnce('Converting').mockResolvedValueOnce(rendered),
      waitUntil: async (predicate, options) => {
        expect(options.timeout).toBe(60_000);
        expect(options.interval).toBe(500);
        expect(await predicate()).toBe(false);
        expect(await predicate()).toBe(true);
      }
    };
    await waitForTerminalQueueStatus({ $: async () => status }, expected, 60_000);
  });

  it.each(['Error', 'Interrupted', 'Cancelled'])(
    'rejects terminal %s instead of accepting success',
    async (rendered) => {
      const status = {
        getText: async () => rendered,
        waitUntil: async (predicate) => expect(await predicate()).toBe(true)
      };
      await expect(
        waitForTerminalQueueStatus({ $: async () => status }, 'completed', 60_000)
      ).rejects.toThrow(`ended in ${rendered.toLowerCase()}, expected completed`);
    }
  );
});

it('waits for startup readiness and a newly added row instead of an existing row', async () => {
  const events = [];
  let rows = [{}];
  let releaseStartup;
  const startup = new Promise((resolve) => (releaseStartup = resolve));
  const button = {
    waitForClickable: vi.fn(async () => {
      events.push('waiting');
      await startup;
    }),
    click: vi.fn(async () => events.push('clicked'))
  };
  const input = { setValue: vi.fn(async () => events.push('filled')) };
  vi.stubGlobal('$', (selector) =>
    selector === 'h1'
      ? { waitForDisplayed: async () => events.push('heading') }
      : selector === '#video-url'
        ? input
        : button
  );
  vi.stubGlobal('$$', async () => rows);
  vi.stubGlobal('browser', {
    waitUntil: async (predicate) => {
      expect(await predicate()).toBe(false);
      rows = [{}, {}];
      expect(await predicate()).toBe(true);
    }
  });
  const adding = addUrl('http://fixture.test/video.mp4');
  await vi.waitFor(() => expect(events).toContain('waiting'));
  expect(button.click).not.toHaveBeenCalled();
  expect(input.setValue).not.toHaveBeenCalled();
  releaseStartup();
  await adding;
  expect(events).toEqual(['heading', 'waiting', 'filled', 'waiting', 'clicked']);
});

describe('startup failure', () => {
  it('does not enter a link or click when the app never becomes usable', async () => {
    const click = vi.fn();
    vi.stubGlobal('$', (selector) =>
      selector === 'h1'
        ? { waitForDisplayed: async () => {} }
        : {
            waitForClickable: async () => {
              throw new Error('startup blocked');
            },
            click
          }
    );
    await expect(addUrl('http://fixture.test/video.mp4')).rejects.toThrow('startup blocked');
    expect(click).not.toHaveBeenCalled();
  });
});
