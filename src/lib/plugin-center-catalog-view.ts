export const pluginCatalogPreviewLimit = 6;

export type PluginCatalogViewMode = "preview" | "all" | "filtered";

export interface PluginCatalogViewState {
  expanded: boolean;
  filter: string;
  query: string;
}

/**
 * The marketplace landing page is the only bounded view. Search and category
 * filters must always expose every matching item, while an explicit
 * "all plugins" action keeps the unfiltered catalog expanded until the user
 * contracts it again.
 */
export function pluginCatalogViewMode({
  expanded,
  filter,
  query,
}: PluginCatalogViewState): PluginCatalogViewMode {
  if (filter !== "all" || query.trim()) {
    return "filtered";
  }
  return expanded ? "all" : "preview";
}

export function pluginCatalogItemsForView<T>(
  items: readonly T[],
  mode: PluginCatalogViewMode,
  previewLimit = pluginCatalogPreviewLimit,
): T[] {
  return mode === "preview" ? items.slice(0, previewLimit) : [...items];
}
