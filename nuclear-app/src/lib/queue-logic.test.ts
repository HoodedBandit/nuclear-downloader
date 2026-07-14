import { describe, expect, it } from "vitest";
import { isUpdateBlockingStatus, type QueueLifecycleStatus } from "./queue-logic";

describe("update safety gating", () => {
  it.each<QueueLifecycleStatus>([
    "fetching",
    "queued",
    "downloading",
    "postprocessing",
  ])("blocks updates while %s work can still own a child process", (status) => {
    expect(isUpdateBlockingStatus(status)).toBe(true);
  });

  it.each<QueueLifecycleStatus>([
    "ready",
    "completed",
    "error",
    "cancelled",
  ])("allows updates for the inert %s state", (status) => {
    expect(isUpdateBlockingStatus(status)).toBe(false);
  });
});
