import { describe, expect, it } from 'vitest';
import { mediaIdentityKey, sameMediaIdentity } from './media-identity';

describe('media identity', () => {
  it('keeps ordinary URLs URL-identical and selected siblings distinct', () => {
    const url = 'https://example.com/parent';
    const first = {
      url,
      selection: { entryId: 'one', extractorKey: 'twitter', playlistIndex: 1 }
    };
    const second = {
      url,
      selection: { entryId: 'two', extractorKey: 'twitter', playlistIndex: 2 }
    };
    expect(sameMediaIdentity({ url }, { url, selection: null })).toBe(true);
    expect(mediaIdentityKey(first)).not.toBe(mediaIdentityKey(second));
    expect(mediaIdentityKey(first)).not.toBe(mediaIdentityKey({ url }));
  });

  it('does not use the display ordinal as durable logical identity', () => {
    const url = 'https://example.com/parent';
    expect(
      sameMediaIdentity(
        { url, selection: { entryId: 'one', extractorKey: 'twitter', playlistIndex: 1 } },
        { url, selection: { entryId: 'one', extractorKey: 'twitter', playlistIndex: 9 } }
      )
    ).toBe(true);
  });
});
