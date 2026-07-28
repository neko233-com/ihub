import { describe, expect, it } from "vitest";
import { activeRecordingElapsedMs, remainingActiveRecordingMs } from "./recording-timing";

describe("active recording timing", () => {
  it("counts only the current active segment on top of committed recording time", () => {
    expect(activeRecordingElapsedMs(10 * 60_000, 1_000, 4_000)).toBe(10 * 60_000 + 3_000);
    expect(activeRecordingElapsedMs(10 * 60_000, null, 40 * 60_000)).toBe(10 * 60_000);
  });

  it("keeps the thirty-minute deadline stable across a long pause", () => {
    const limit = 30 * 60_000;
    const committedAfterFirstSegment = activeRecordingElapsedMs(0, 0, 10 * 60_000);
    expect(committedAfterFirstSegment).toBe(10 * 60_000);
    expect(remainingActiveRecordingMs(limit, committedAfterFirstSegment, null, 50 * 60_000)).toBe(20 * 60_000);
    expect(remainingActiveRecordingMs(limit, committedAfterFirstSegment, 50 * 60_000, 70 * 60_000)).toBe(0);
  });

  it("never creates time from a backwards clock or a negative limit", () => {
    expect(activeRecordingElapsedMs(2_000, 10_000, 9_000)).toBe(2_000);
    expect(remainingActiveRecordingMs(-1, 0, null, 0)).toBe(0);
  });
});
