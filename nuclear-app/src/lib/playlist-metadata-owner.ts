export const MAX_PLAYLIST_METADATA_ENTRIES = 1_000;

export interface PlaylistMetadataValue {
  duration: number | null | undefined;
  channel: string | null | undefined;
  thumbnail: string | null | undefined;
}

export interface PlaylistMetadataEntry {
  identity: string;
  metadata: PlaylistMetadataValue;
}

export interface PlaylistMetadataLease {
  confirm(itemIds: readonly string[]): void;
  discard(): void;
}

interface ActiveLease {
  tokens: Map<string, PlaylistMetadataValue>;
  outstandingIds: Set<string> | null;
  active: boolean;
}

export class PlaylistMetadataOwner<Record> {
  private readonly leases = new Set<ActiveLease>();
  private retainedEntryCount = 0;

  constructor(
    private readonly findRecord: (itemId: string) => Record | undefined,
    private readonly identityForRecord: (record: Record) => string,
    private readonly applyMetadata: (itemId: string, metadata: PlaylistMetadataValue) => boolean
  ) {}

  retain(entries: readonly PlaylistMetadataEntry[]): PlaylistMetadataLease {
    const tokens = new Map<string, PlaylistMetadataValue>();
    for (const entry of entries) {
      const existing = tokens.get(entry.identity);
      tokens.set(
        entry.identity,
        existing
          ? {
              duration: existing.duration ?? entry.metadata.duration,
              channel: existing.channel ?? entry.metadata.channel,
              thumbnail: existing.thumbnail ?? entry.metadata.thumbnail
            }
          : entry.metadata
      );
    }
    if (
      tokens.size > MAX_PLAYLIST_METADATA_ENTRIES ||
      this.retainedEntryCount + tokens.size > MAX_PLAYLIST_METADATA_ENTRIES
    ) {
      throw new Error('Playlist display metadata exceeded its retention limit.');
    }

    if (tokens.size === 0) {
      return { confirm: () => {}, discard: () => {} };
    }

    const lease: ActiveLease = { tokens, outstandingIds: null, active: true };
    this.leases.add(lease);
    this.retainedEntryCount += tokens.size;

    return {
      confirm: (itemIds) => this.confirm(lease, itemIds),
      discard: () => this.release(lease)
    };
  }

  reconcile(authoritativeAbsence = false): void {
    for (const lease of [...this.leases]) this.reconcileLease(lease, authoritativeAbsence);
  }

  remove(itemIds: readonly string[]): void {
    const removed = new Set(itemIds);
    for (const lease of [...this.leases]) {
      if (!lease.outstandingIds) continue;
      for (const itemId of removed) lease.outstandingIds.delete(itemId);
      if (lease.outstandingIds.size === 0) this.release(lease);
    }
  }

  dispose(): void {
    for (const lease of [...this.leases]) this.release(lease);
  }

  private confirm(lease: ActiveLease, itemIds: readonly string[]): void {
    if (!lease.active) return;
    const uniqueIds = new Set(itemIds);
    if (
      itemIds.length > MAX_PLAYLIST_METADATA_ENTRIES ||
      itemIds.length > lease.tokens.size ||
      uniqueIds.size !== itemIds.length ||
      lease.outstandingIds !== null
    ) {
      this.release(lease);
      throw new Error('Playlist admission returned invalid metadata ownership.');
    }
    lease.outstandingIds = uniqueIds;
    this.reconcileLease(lease, false);
  }

  private reconcileLease(lease: ActiveLease, authoritativeAbsence: boolean): void {
    if (!lease.active || lease.outstandingIds === null) return;
    for (const itemId of [...lease.outstandingIds]) {
      const record = this.findRecord(itemId);
      if (!record) {
        if (authoritativeAbsence) lease.outstandingIds.delete(itemId);
        continue;
      }
      const identity = this.identityForRecord(record);
      const metadata = lease.tokens.get(identity);
      if (metadata && !this.applyMetadata(itemId, metadata)) continue;
      if (metadata) {
        lease.tokens.delete(identity);
        this.retainedEntryCount -= 1;
      }
      lease.outstandingIds.delete(itemId);
    }
    if (lease.outstandingIds.size === 0 || lease.tokens.size === 0) this.release(lease);
  }

  private release(lease: ActiveLease): void {
    if (!lease.active) return;
    lease.active = false;
    this.retainedEntryCount -= lease.tokens.size;
    lease.tokens.clear();
    lease.outstandingIds?.clear();
    this.leases.delete(lease);
  }
}
