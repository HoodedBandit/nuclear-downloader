import { describe, expect, it, vi } from 'vitest';
import {
  MAX_PLAYLIST_METADATA_ENTRIES,
  PlaylistMetadataOwner,
  type PlaylistMetadataEntry
} from './playlist-metadata-owner';

interface RecordFixture {
  id: string;
  identity: string;
}

const metadata = (duration: number): PlaylistMetadataEntry => ({
  identity: `identity-${duration}`,
  metadata: { duration, channel: `channel-${duration}`, thumbnail: null }
});

function setup() {
  const records = new Map<string, RecordFixture>();
  const apply = vi.fn(() => true);
  const owner = new PlaylistMetadataOwner(
    (id) => records.get(id),
    (record) => record.identity,
    apply
  );
  return { owner, records, apply };
}

describe('PlaylistMetadataOwner', () => {
  it('waits for a confirmed id and a later record before applying metadata', () => {
    const { owner, records, apply } = setup();
    const lease = owner.retain([metadata(1)]);
    records.set('existing', { id: 'existing', identity: 'identity-1' });
    owner.reconcile();
    expect(apply).not.toHaveBeenCalled();

    lease.confirm(['confirmed']);
    owner.reconcile();
    expect(apply).not.toHaveBeenCalled();
    records.set('confirmed', { id: 'confirmed', identity: 'identity-1' });
    owner.reconcile();
    expect(apply).toHaveBeenCalledOnce();
    expect(apply).toHaveBeenCalledWith('confirmed', metadata(1).metadata);
  });

  it('applies immediately when the confirmed record predates the lease', () => {
    const { owner, records, apply } = setup();
    records.set('replayed', { id: 'replayed', identity: 'identity-2' });
    const lease = owner.retain([metadata(2)]);
    lease.confirm(['replayed']);
    expect(apply).toHaveBeenCalledWith('replayed', metadata(2).metadata);
  });

  it('discards skipped and duplicate identity tokens after confirmed ids settle', () => {
    const { owner, records, apply } = setup();
    const lease = owner.retain([metadata(3), metadata(3), metadata(4)]);
    records.set('accepted', { id: 'accepted', identity: 'identity-4' });
    lease.confirm(['accepted']);
    records.set('later', { id: 'later', identity: 'identity-3' });
    owner.reconcile();
    expect(apply).toHaveBeenCalledTimes(1);
    expect(apply).toHaveBeenCalledWith('accepted', metadata(4).metadata);
  });

  it('merges richer fields from duplicate identities', () => {
    const { owner, records, apply } = setup();
    const lease = owner.retain([
      { identity: 'same', metadata: { duration: null, channel: null, thumbnail: 'thumb' } },
      { identity: 'same', metadata: { duration: 9, channel: 'channel', thumbnail: null } }
    ]);
    records.set('accepted', { id: 'accepted', identity: 'same' });
    lease.confirm(['accepted']);
    expect(apply).toHaveBeenCalledWith('accepted', {
      duration: 9,
      channel: 'channel',
      thumbnail: 'thumb'
    });
  });

  it('settles removed confirmed ids and clears state on disposal', () => {
    const { owner, records, apply } = setup();
    const removed = owner.retain([metadata(5)]);
    removed.confirm(['removed']);
    owner.remove(['removed']);
    records.set('removed', { id: 'removed', identity: 'identity-5' });
    owner.reconcile();

    const disposed = owner.retain([metadata(6)]);
    disposed.confirm(['disposed']);
    owner.dispose();
    records.set('disposed', { id: 'disposed', identity: 'identity-6' });
    owner.reconcile();
    expect(apply).not.toHaveBeenCalled();
  });

  it('settles confirmed ids absent from a later authoritative snapshot', () => {
    const { owner, records, apply } = setup();
    owner.retain([metadata(10)]).confirm(['missing']);
    owner.reconcile(true);
    records.set('missing', { id: 'missing', identity: 'identity-10' });
    owner.reconcile();
    expect(apply).not.toHaveBeenCalled();
  });

  it('fails closed for oversized or duplicate confirmations without leaking capacity', () => {
    const { owner } = setup();
    expect(() =>
      owner.retain(
        Array.from({ length: MAX_PLAYLIST_METADATA_ENTRIES + 1 }, (_, index) => metadata(index))
      )
    ).toThrow(/retention limit/);

    const invalid = owner.retain([metadata(7)]);
    expect(() => invalid.confirm(['same', 'same'])).toThrow(/invalid metadata ownership/);
    const tooManyIds = owner.retain([metadata(9)]);
    expect(() => tooManyIds.confirm(['one', 'two'])).toThrow(/invalid metadata ownership/);
    const full = owner.retain(
      Array.from({ length: MAX_PLAYLIST_METADATA_ENTRIES }, (_, index) => metadata(index))
    );
    full.discard();
  });

  it('returns consumed quota and does not retain empty leases', () => {
    const { owner, records } = setup();
    for (let index = 0; index < MAX_PLAYLIST_METADATA_ENTRIES * 2; index += 1) {
      owner.retain([]).confirm([]);
    }
    records.set('consumed', { id: 'consumed', identity: 'identity-8' });
    owner.retain([metadata(8)]).confirm(['consumed']);
    const full = owner.retain(
      Array.from({ length: MAX_PLAYLIST_METADATA_ENTRIES }, (_, index) => metadata(index))
    );
    full.discard();
  });
});
