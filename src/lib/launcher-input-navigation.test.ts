import { describe, expect, it } from "vitest";
import { launcherInputUsesHorizontalGridNavigation } from "./launcher-input-navigation";

describe("launcher input horizontal navigation", () => {
  it("uses the tile grid only when the search input is empty", () => {
    expect(launcherInputUsesHorizontalGridNavigation("")).toBe(true);
  });

  it("preserves native caret movement for every non-empty query", () => {
    expect(launcherInputUsesHorizontalGridNavigation("Rider")).toBe(false);
    expect(launcherInputUsesHorizontalGridNavigation(" Rider ")).toBe(false);
    expect(launcherInputUsesHorizontalGridNavigation(" ")).toBe(false);
  });
});
