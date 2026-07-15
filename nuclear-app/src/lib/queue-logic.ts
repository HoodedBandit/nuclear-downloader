export type QueueLifecycleStatus =
  | "fetching"
  | "ready"
  | "queued"
  | "downloading"
  | "postprocessing"
  | "completed"
  | "error"
  | "cancelled";

export function isUpdateBlockingStatus(status: QueueLifecycleStatus): boolean {
  return (
    status === "fetching" ||
    status === "queued" ||
    status === "downloading" ||
    status === "postprocessing"
  );
}

export function findNextRunnablePendingId(
  pendingIds: string[],
  priorityId: string | null,
  editingId: string | null,
): string | null {
  const candidates = priorityId ? [priorityId, ...pendingIds] : pendingIds;
  return candidates.find((itemId) => itemId !== editingId) ?? null;
}

export function resolveAvailableQuality(
  requestedQuality: string,
  availableQualities: string[],
): string {
  return availableQualities.includes(requestedQuality) ? requestedQuality : "best";
}
