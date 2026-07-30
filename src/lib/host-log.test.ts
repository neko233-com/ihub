import { describe, expect, it } from "vitest";
import {
  browserHostLogSnapshot,
  canClearHostLog,
  emptyHostLogSnapshot,
  formatHostLogForClipboard,
  normalizeHostLogSnapshot,
} from "./host-log";

describe("bounded host log renderer projection", () => {
  it("uses a browser-only fixture without local paths or secret-like values", () => {
    const fixture = browserHostLogSnapshot();
    const encoded = JSON.stringify(fixture);
    expect(fixture.entries).toHaveLength(3);
    expect(encoded).not.toMatch(/[A-Za-z]:[\\/]/);
    expect(encoded).not.toContain("/Users/");
    expect(encoded).not.toMatch(/password\s*[:=]/i);
    expect(encoded).not.toMatch(/token\s*[:=]/i);
  });

  it("bounds native-shaped entries again before displaying or copying them", () => {
    const snapshot = normalizeHostLogSnapshot({
      generatedAt: "invalid",
      entries: Array.from({ length: 1_050 }, (_, index) => ({
        timestamp: "invalid",
        level: index % 2 ? "error" : "info",
        component: "x".repeat(80),
        message: `${index}:${"m".repeat(3_000)}`,
      })),
      truncated: false,
      totalBytes: Number.POSITIVE_INFINITY,
      activeFileBytes: -1,
      maxFileBytes: 262_144,
      maxFiles: 400,
      writeFailures: -10,
    });
    expect(snapshot.entries).toHaveLength(1_000);
    expect(snapshot.truncated).toBe(true);
    expect(snapshot.entries[0]?.component).toHaveLength(48);
    expect(snapshot.entries[0]?.message.length).toBeLessThanOrEqual(2_049);
    expect(snapshot.totalBytes).toBe(0);
    expect(snapshot.activeFileBytes).toBe(0);
    expect(snapshot.maxFiles).toBe(16);
    expect(snapshot.writeFailures).toBe(0);
  });

  it("formats a self-describing copy and clears only the in-memory projection", () => {
    const fixture = browserHostLogSnapshot();
    const copied = formatHostLogForClipboard(fixture);
    expect(copied).toContain("# iHub bounded host diagnostics");
    expect(copied).toContain("[lifecycle]");
    const cleared = emptyHostLogSnapshot(fixture);
    expect(cleared.entries).toEqual([]);
    expect(cleared.totalBytes).toBe(0);
    expect(fixture.entries).toHaveLength(3);
  });

  it("keeps clear available as recovery for unreadable or malformed retained files", () => {
    const empty = emptyHostLogSnapshot(browserHostLogSnapshot());
    expect(canClearHostLog(null, "Retained log exceeded its read limit.")).toBe(true);
    expect(canClearHostLog({
      ...empty,
      totalBytes: 19,
      activeFileBytes: 19,
    }, null)).toBe(true);
    expect(canClearHostLog(empty, null)).toBe(false);
  });
});
