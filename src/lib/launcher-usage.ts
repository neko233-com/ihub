export const LAUNCHER_USAGE_CAPACITY = 256;
export const LAUNCHER_USAGE_RETENTION_MS = 180 * 24 * 60 * 60 * 1_000;
export const LAUNCHER_USAGE_HALF_LIFE_MS = 14 * 24 * 60 * 60 * 1_000;

const MAX_RAW_USAGE_ID_LENGTH = 8 * 1024;
const MAX_USAGE_COUNT = 1_000_000;
const MAX_DECAYED_SCORE = 1_024;
const USAGE_ID_PREFIX = "usage-v1:";
const USAGE_ID_PATTERN = /^usage-v1:[0-9a-f]{16}:[0-9a-f]{16}$/;
const FNV64_PRIME = 0x100000001b3n;
const FNV64_MASK = 0xffffffffffffffffn;
const FNV64_OFFSET_A = 0xcbf29ce484222325n;
const FNV64_OFFSET_B = 0x84222325cbf29ce4n;
const usageIdEncoder = new TextEncoder();

export interface LauncherUsageEntry {
  id: string;
  uses: number;
  score: number;
  lastUsedAt: number;
  updatedAt: number;
}

export type LauncherUsageLedger = LauncherUsageEntry[];

function safeTimestamp(value: unknown, now: number): number | null {
  if (
    typeof value !== "number"
    || !Number.isFinite(value)
    || value < 0
    || value > now + 5 * 60 * 1_000
  ) {
    return null;
  }
  return Math.floor(value);
}

function safeRawItemId(value: unknown): value is string {
  return typeof value === "string"
    && value.length > 0
    && value.length <= MAX_RAW_USAGE_ID_LENGTH
    && value.trim() === value
    && ![...value].some((character) => character < " " || character === "\u007f");
}

function safeStoredUsageId(value: unknown): value is string {
  return typeof value === "string" && USAGE_ID_PATTERN.test(value);
}

function fnv1a64(bytes: Uint8Array, offset: bigint, reverse: boolean): string {
  let hash = offset;
  for (let step = 0; step < bytes.length; step += 1) {
    const index = reverse ? bytes.length - step - 1 : step;
    hash ^= BigInt(bytes[index] ?? 0);
    hash = (hash * FNV64_PRIME) & FNV64_MASK;
  }
  return hash.toString(16).padStart(16, "0");
}

/**
 * Produces a stable, local pseudonymous identity. The stored ledger never
 * contains the original result ID, which can itself be an absolute path.
 */
export function launcherUsageIdentity(id: string): string | null {
  if (!safeRawItemId(id)) {
    return null;
  }
  const bytes = usageIdEncoder.encode(`ihub-launcher-usage-v1\0${id}`);
  return `${USAGE_ID_PREFIX}${fnv1a64(bytes, FNV64_OFFSET_A, false)}:${
    fnv1a64(bytes, FNV64_OFFSET_B, true)
  }`;
}

export function launcherUsageWeight(entry: LauncherUsageEntry, now: number): number {
  const age = Math.max(0, now - entry.updatedAt);
  return entry.score * Math.pow(0.5, age / LAUNCHER_USAGE_HALF_LIFE_MS);
}

/**
 * Reads only pseudonymous launcher identities and bounded counters. Paths,
 * queries, clipboard contents, and plugin payloads are never persisted here.
 */
export function parseLauncherUsageLedger(
  value: unknown,
  now = Date.now(),
): LauncherUsageLedger {
  if (!Array.isArray(value)) {
    return [];
  }
  const byId = new Map<string, LauncherUsageEntry>();
  for (const candidate of value.slice(0, LAUNCHER_USAGE_CAPACITY * 2)) {
    if (!candidate || typeof candidate !== "object" || Array.isArray(candidate)) {
      continue;
    }
    const record = candidate as Partial<LauncherUsageEntry>;
    const lastUsedAt = safeTimestamp(record.lastUsedAt, now);
    const updatedAt = safeTimestamp(record.updatedAt, now);
    if (
      !safeStoredUsageId(record.id)
      || lastUsedAt === null
      || updatedAt === null
      || now - lastUsedAt > LAUNCHER_USAGE_RETENTION_MS
      || typeof record.uses !== "number"
      || !Number.isInteger(record.uses)
      || record.uses < 1
      || record.uses > MAX_USAGE_COUNT
      || typeof record.score !== "number"
      || !Number.isFinite(record.score)
      || record.score <= 0
      || record.score > MAX_DECAYED_SCORE
    ) {
      continue;
    }
    const normalized = {
      id: record.id,
      uses: record.uses,
      score: record.score,
      lastUsedAt,
      updatedAt,
    };
    const existing = byId.get(normalized.id);
    if (!existing || existing.updatedAt < normalized.updatedAt) {
      byId.set(normalized.id, normalized);
    }
  }
  return [...byId.values()]
    .sort((left, right) =>
      launcherUsageWeight(right, now) - launcherUsageWeight(left, now)
      || right.lastUsedAt - left.lastUsedAt
      || left.id.localeCompare(right.id),
    )
    .slice(0, LAUNCHER_USAGE_CAPACITY);
}

export function recordLauncherUsage(
  ledger: LauncherUsageLedger,
  id: string,
  now = Date.now(),
): LauncherUsageLedger {
  const usageId = launcherUsageIdentity(id);
  if (!usageId || !Number.isFinite(now) || now < 0) {
    return ledger;
  }
  const current = ledger.find((entry) => entry.id === usageId);
  const next: LauncherUsageEntry = {
    id: usageId,
    uses: Math.min(MAX_USAGE_COUNT, (current?.uses ?? 0) + 1),
    score: Math.min(
      MAX_DECAYED_SCORE,
      (current ? launcherUsageWeight(current, now) : 0) + 1,
    ),
    lastUsedAt: Math.floor(now),
    updatedAt: Math.floor(now),
  };
  return parseLauncherUsageLedger(
    [next, ...ledger.filter((entry) => entry.id !== usageId)],
    now,
  );
}

/**
 * Search relevance remains primary: this bounded boost can reorder similarly
 * matching candidates, but cannot overtake the 1,000-point exact/prefix tiers.
 */
export function launcherUsageSearchBoost(
  ledger: LauncherUsageLedger,
  id: string,
  now = Date.now(),
): number {
  const usageId = launcherUsageIdentity(id);
  const entry = usageId
    ? ledger.find((candidate) => candidate.id === usageId)
    : undefined;
  if (!entry) {
    return 0;
  }
  const weight = launcherUsageWeight(entry, now);
  const recency = Math.pow(
    0.5,
    Math.max(0, now - entry.lastUsedAt) / LAUNCHER_USAGE_HALF_LIFE_MS,
  );
  return Math.min(480, Math.log2(1 + weight) * 135 + recency * 75);
}

export function sortLauncherItemsByUsage<T extends { id: string }>(
  items: readonly T[],
  ledger: LauncherUsageLedger,
  now = Date.now(),
): T[] {
  const usage = new Map(ledger.map((entry) => [entry.id, entry] as const));
  return items
    .map((item, order) => {
      const usageId = launcherUsageIdentity(item.id);
      return { item, order, entry: usageId ? usage.get(usageId) : undefined };
    })
    .sort((left, right) => {
      const leftWeight = left.entry ? launcherUsageWeight(left.entry, now) : 0;
      const rightWeight = right.entry ? launcherUsageWeight(right.entry, now) : 0;
      return rightWeight - leftWeight
        || (right.entry?.lastUsedAt ?? 0) - (left.entry?.lastUsedAt ?? 0)
        || left.order - right.order;
    })
    .map(({ item }) => item);
}
