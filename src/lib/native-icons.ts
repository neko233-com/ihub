const nativePngPrefix = "data:image/png;base64,";
export const MAX_NATIVE_ICON_DATA_URL_BYTES = 128 * 1024;

export interface SystemIconRequest {
  searchResultIds: string[];
  launcherShortcutIds: string[];
}

export type SystemIconMap = Record<string, string>;
export const MAX_NATIVE_ICON_CACHE_ENTRIES = 384;

export interface NativeIconResultIdentity {
  id: string;
  kind: "file" | "folder" | "application" | string;
  path?: string;
}

const resultCacheKeyPrefix = "result:";
const resultPathCacheKeyPrefix = "path:";
const shortcutCacheKeyPrefix = "shortcut:";

export function safeNativeIconSrc(value: unknown): string | undefined {
  if (
    typeof value !== "string"
    || value.length > MAX_NATIVE_ICON_DATA_URL_BYTES
    || !value.startsWith(nativePngPrefix)
  ) {
    return undefined;
  }
  const payload = value.slice(nativePngPrefix.length);
  return payload.startsWith("iVBORw0KGgo")
    && payload.length % 4 === 0
    && /^[A-Za-z0-9+/]+={0,2}$/.test(payload)
    ? value
    : undefined;
}

export function sanitizeSystemIconMap(
  value: unknown,
  allowedIds: ReadonlySet<string>,
): SystemIconMap {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return {};
  }
  const safe: SystemIconMap = {};
  for (const [id, iconSrc] of Object.entries(value)) {
    if (!allowedIds.has(id)) {
      continue;
    }
    const normalized = safeNativeIconSrc(iconSrc);
    if (normalized) {
      safe[id] = normalized;
    }
  }
  return safe;
}

function stableLocalPath(value: unknown): string | undefined {
  if (typeof value !== "string") {
    return undefined;
  }
  const trimmed = value.trim();
  if (!trimmed || trimmed.length > 32_768 || trimmed.includes("\0")) {
    return undefined;
  }
  if (/^[a-z]:[\\/]/i.test(trimmed) || /^\\\\/.test(trimmed)) {
    return trimmed.replaceAll("/", "\\").toLowerCase();
  }
  return trimmed.startsWith("/") ? trimmed : undefined;
}

function resultCacheKeys(result: NativeIconResultIdentity): string[] {
  if (
    !result.id
    || (
      result.kind !== "application"
      && result.kind !== "file"
      && result.kind !== "folder"
    )
  ) {
    return [];
  }
  const path = stableLocalPath(result.path);
  if (!path) {
    return [`${resultCacheKeyPrefix}${result.kind}:${result.id}`];
  }
  return [
    `${resultCacheKeyPrefix}${result.kind}:${result.id}\n${path}`,
    `${resultPathCacheKeyPrefix}${result.kind}:${path}`,
  ];
}

function shortcutCacheKey(shortcutId: string): string {
  return `${shortcutCacheKeyPrefix}${shortcutId}`;
}

/**
 * Keeps already-rendered shell artwork stable while a refreshed result set is
 * waiting on native extraction. Search IDs remain the primary identity; an
 * absolute local path is also retained so the same target can reuse its icon
 * when the index assigns a new response ID. Both dimensions are host-derived.
 */
export function mergeNativeIconCache(
  current: SystemIconMap,
  incoming: SystemIconMap,
  results: readonly NativeIconResultIdentity[] = [],
  launcherShortcutIds: readonly string[] = [],
  limit = MAX_NATIVE_ICON_CACHE_ENTRIES,
): SystemIconMap {
  if (!Number.isSafeInteger(limit) || limit < 1) {
    return {};
  }
  const cache = new Map<string, string>();
  const touch = (key: string, iconSrc: string) => {
    cache.delete(key);
    cache.set(key, iconSrc);
  };

  for (const [key, value] of Object.entries(current)) {
    const iconSrc = safeNativeIconSrc(value);
    if (iconSrc) {
      touch(key, iconSrc);
    }
  }
  for (const result of results) {
    const iconSrc = safeNativeIconSrc(incoming[result.id]);
    if (!iconSrc) {
      continue;
    }
    for (const key of resultCacheKeys(result)) {
      touch(key, iconSrc);
    }
  }
  for (const shortcutId of launcherShortcutIds) {
    const iconSrc = safeNativeIconSrc(incoming[shortcutId]);
    if (shortcutId && iconSrc) {
      touch(shortcutCacheKey(shortcutId), iconSrc);
    }
  }

  while (cache.size > limit) {
    const oldestKey = cache.keys().next().value;
    if (typeof oldestKey !== "string") {
      break;
    }
    cache.delete(oldestKey);
  }
  return Object.fromEntries(cache);
}

export function nativeIconForResult(
  cache: SystemIconMap,
  result: NativeIconResultIdentity,
): string | undefined {
  for (const key of resultCacheKeys(result)) {
    const iconSrc = safeNativeIconSrc(cache[key]);
    if (iconSrc) {
      return iconSrc;
    }
  }
  return undefined;
}

export function nativeIconForLauncherShortcut(
  cache: SystemIconMap,
  shortcutId: string,
): string | undefined {
  return safeNativeIconSrc(cache[shortcutCacheKey(shortcutId)]);
}

export function systemIconRequestChunks(
  searchResultIds: readonly string[],
  launcherShortcutIds: readonly string[],
  limit = 12,
): SystemIconRequest[] {
  if (!Number.isSafeInteger(limit) || limit < 1) {
    return [];
  }
  const seen = new Set<string>();
  const targets = [
    ...searchResultIds.map((id) => ({ id, kind: "search" as const })),
    ...launcherShortcutIds.map((id) => ({ id, kind: "shortcut" as const })),
  ].filter(({ id }) => {
    if (!id || seen.has(id)) {
      return false;
    }
    seen.add(id);
    return true;
  });

  const chunks: SystemIconRequest[] = [];
  for (let offset = 0; offset < targets.length; offset += limit) {
    const chunk = targets.slice(offset, offset + limit);
    chunks.push({
      searchResultIds: chunk
        .filter(({ kind }) => kind === "search")
        .map(({ id }) => id),
      launcherShortcutIds: chunk
        .filter(({ kind }) => kind === "shortcut")
        .map(({ id }) => id),
    });
  }
  return chunks;
}
