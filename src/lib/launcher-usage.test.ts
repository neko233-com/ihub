import { describe, expect, it } from "vitest";
import {
  LAUNCHER_USAGE_HALF_LIFE_MS,
  LAUNCHER_USAGE_RETENTION_MS,
  launcherUsageIdentity,
  launcherUsageSearchBoost,
  parseLauncherUsageLedger,
  recordLauncherUsage,
  sortLauncherItemsByUsage,
} from "./launcher-usage";

describe("local adaptive launcher usage", () => {
  it("records only bounded pseudonymous IDs and ignores stale or malformed storage", () => {
    const now = 2_000_000_000_000;
    const jsonUsageId = launcherUsageIdentity("builtin-command:json");
    expect(jsonUsageId).not.toBeNull();
    const parsed = parseLauncherUsageLedger([
      {
        id: jsonUsageId,
        uses: 4,
        score: 3.5,
        lastUsedAt: now - 1_000,
        updatedAt: now - 1_000,
      },
      {
        id: launcherUsageIdentity("stale"),
        uses: 100,
        score: 100,
        lastUsedAt: now - LAUNCHER_USAGE_RETENTION_MS - 1,
        updatedAt: now - LAUNCHER_USAGE_RETENTION_MS - 1,
      },
      {
        id: launcherUsageIdentity("bad"),
        uses: -1,
        score: Number.NaN,
        lastUsedAt: now,
        updatedAt: now,
      },
      {
        id: "C:\\Users\\person\\private\\notes.txt",
        uses: 2,
        score: 2,
        lastUsedAt: now,
        updatedAt: now,
      },
    ], now);

    expect(parsed.map((entry) => entry.id)).toEqual([jsonUsageId]);
  });

  it("never serializes a native result path into the local usage ledger", () => {
    const now = 2_000_000_000_000;
    const path = "C:\\Users\\person\\private\\notes.txt";
    const ledger = recordLauncherUsage([], path, now);
    const serialized = JSON.stringify(ledger);

    expect(ledger[0]?.id).toMatch(/^usage-v1:[0-9a-f]{16}:[0-9a-f]{16}$/);
    expect(serialized).not.toContain(path);
    expect(serialized).not.toContain("Users");
    expect(launcherUsageSearchBoost(ledger, path, now)).toBeGreaterThan(0);
  });

  it("lets recent repeated use outrank one-off recency, then decays old habits", () => {
    const now = 2_000_000_000_000;
    let ledger = recordLauncherUsage([], "frequent", now - 2_000);
    ledger = recordLauncherUsage(ledger, "frequent", now - 1_500);
    ledger = recordLauncherUsage(ledger, "frequent", now - 1_000);
    ledger = recordLauncherUsage(ledger, "single", now);

    expect(sortLauncherItemsByUsage(
      [{ id: "single" }, { id: "frequent" }],
      ledger,
      now,
    ).map((item) => item.id)).toEqual(["frequent", "single"]);

    expect(
      launcherUsageSearchBoost(ledger, "frequent", now + LAUNCHER_USAGE_HALF_LIFE_MS * 8),
    ).toBeLessThan(launcherUsageSearchBoost(ledger, "single", now));
  });

  it("deduplicates persisted identities and keeps the newest valid update", () => {
    const now = 2_000_000_000_000;
    const sameUsageId = launcherUsageIdentity("same");
    const parsed = parseLauncherUsageLedger([
      {
        id: sameUsageId,
        uses: 2,
        score: 2,
        lastUsedAt: now - 10,
        updatedAt: now - 10,
      },
      {
        id: sameUsageId,
        uses: 1,
        score: 1,
        lastUsedAt: now - 20,
        updatedAt: now - 20,
      },
    ], now);
    expect(parsed).toHaveLength(1);
    expect(parsed[0]?.uses).toBe(2);
  });
});
