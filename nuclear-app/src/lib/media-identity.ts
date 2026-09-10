import type { MediaSelection } from './bindings/MediaSelection';

export interface MediaIdentity {
  url: string;
  selection?: MediaSelection | null;
}

export function mediaIdentityKey(media: MediaIdentity): string {
  const selection = media.selection;
  return selection
    ? JSON.stringify([media.url, selection.entryId, selection.extractorKey])
    : JSON.stringify([media.url]);
}

export function sameMediaIdentity(left: MediaIdentity, right: MediaIdentity): boolean {
  return mediaIdentityKey(left) === mediaIdentityKey(right);
}
