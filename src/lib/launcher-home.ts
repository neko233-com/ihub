export const LAUNCHER_RECENT_CAPACITY = 48;

export const LAUNCHER_HOME_PREVIEW_CAPACITY = {
  recent: 9,
  pinned: 18,
  marketplace: 9,
} as const;

export type LauncherHomePreviewGroup = keyof typeof LAUNCHER_HOME_PREVIEW_CAPACITY;

const launcherChromeDestinationIds = new Set([
  "ihub.open-plugin-center",
  "ihub.open-settings",
  "system-command:ihub.open-settings",
]);

/** Keeps the newest launcher history entries inside one shared, explicit bound. */
export function retainLauncherRecent<T>(items: readonly T[]): T[] {
  return items.slice(0, LAUNCHER_RECENT_CAPACITY);
}

/** Home is a preview; a focused recent/pinned view receives the complete list. */
export function launcherHomePreview<T>(
  group: LauncherHomePreviewGroup,
  items: readonly T[],
  expanded = false,
): readonly T[] {
  return expanded ? items : items.slice(0, LAUNCHER_HOME_PREVIEW_CAPACITY[group]);
}

/** Settings and center navigation are shell chrome, not completed user work. */
export function isLauncherRecentDestination(itemId: string): boolean {
  return !launcherChromeDestinationIds.has(itemId);
}

/**
 * Built-in identities are authoritative when base collections overlap.
 * Current live search results are applied afterwards because they represent
 * fresher host state for that exact result ID.
 */
export function buildLauncherItemIndex<T extends { id: string }>(
  baseItems: readonly T[],
  liveItems: readonly T[] = [],
): Map<string, T> {
  const index = new Map<string, T>();
  for (const item of baseItems) {
    if (!index.has(item.id)) {
      index.set(item.id, item);
    }
  }
  for (const item of liveItems) {
    index.set(item.id, item);
  }
  return index;
}
