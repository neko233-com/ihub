import { describe, expect, it } from "vitest";
import {
  pluginCatalogItemsForView,
  pluginCatalogPreviewLimit,
  pluginCatalogViewMode,
} from "./plugin-center-catalog-view";

describe("plugin center catalog view", () => {
  const items = Array.from({ length: pluginCatalogPreviewLimit + 3 }, (_, index) => index);

  it("bounds only the default marketplace preview", () => {
    const mode = pluginCatalogViewMode({
      expanded: false,
      filter: "all",
      query: "",
    });

    expect(mode).toBe("preview");
    expect(pluginCatalogItemsForView(items, mode)).toEqual(
      items.slice(0, pluginCatalogPreviewLimit),
    );
  });

  it("shows the complete catalog after an explicit expand action", () => {
    const mode = pluginCatalogViewMode({
      expanded: true,
      filter: "all",
      query: "   ",
    });

    expect(mode).toBe("all");
    expect(pluginCatalogItemsForView(items, mode)).toEqual(items);
  });

  it("never truncates search or category results", () => {
    expect(pluginCatalogViewMode({
      expanded: false,
      filter: "all",
      query: "json",
    })).toBe("filtered");
    expect(pluginCatalogViewMode({
      expanded: false,
      filter: "developer",
      query: "",
    })).toBe("filtered");
    expect(pluginCatalogItemsForView(items, "filtered")).toEqual(items);
  });

  it("returns a copy instead of mutating the source collection", () => {
    const result = pluginCatalogItemsForView(items, "all");

    expect(result).toEqual(items);
    expect(result).not.toBe(items);
  });
});
