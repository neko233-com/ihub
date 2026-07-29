const nativePngPrefix = "data:image/png;base64,";
export const MAX_NATIVE_ICON_DATA_URL_BYTES = 128 * 1024;

export interface SystemIconRequest {
  searchResultIds: string[];
  launcherShortcutIds: string[];
}

export type SystemIconMap = Record<string, string>;

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
