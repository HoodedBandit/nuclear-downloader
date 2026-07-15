import { describe, expect, it } from "vitest";
import {
  findNextRunnablePendingId,
  isUpdateBlockingStatus,
  resolveAvailableQuality,
  type QueueLifecycleStatus,
} from "./queue-logic";

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

describe("queue scheduling", () => {
  it("skips an edited head item without removing it from consideration", () => {
    expect(findNextRunnablePendingId(["edited", "next"], null, "edited")).toBe("next");
    expect(findNextRunnablePendingId(["edited"], null, "edited")).toBeNull();
  });

  it("preserves priority while allowing other work to run", () => {
    expect(findNextRunnablePendingId(["normal", "priority"], "priority", "priority")).toBe(
      "normal",
    );
    expect(findNextRunnablePendingId(["normal", "priority"], "priority", null)).toBe(
      "priority",
    );
  });
});

describe("quality selection", () => {
  it("falls back to best when a global quality is unavailable", () => {
    expect(resolveAvailableQuality("2160p", ["best", "1080p", "720p"])).toBe("best");
    expect(resolveAvailableQuality("1080p", ["best", "1080p", "720p"])).toBe("1080p");
  });
});
