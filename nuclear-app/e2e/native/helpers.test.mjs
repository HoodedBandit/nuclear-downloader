import { afterEach, describe, expect, it, vi } from 'vitest';
import { addUrl } from './helpers.mjs';

afterEach(() => vi.unstubAllGlobals());

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
