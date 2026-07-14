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
