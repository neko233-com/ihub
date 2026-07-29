import { describe, expect, it } from "vitest";
import {
  buildLauncherItemIndex,
  isLauncherRecentDestination,
  LAUNCHER_RECENT_CAPACITY,
  launcherHomePreview,
  retainLauncherRecent,
} from "./launcher-home";

describe("launcher home data bounds", () => {
  it("retains the newest forty-eight history entries", () => {
    const history = Array.from({ length: 60 }, (_, index) => `item-${index}`);

    expect(retainLauncherRecent(history)).toEqual(history.slice(0, LAUNCHER_RECENT_CAPACITY));
  });

  it("uses exact home quotas while focused groups receive all data", () => {
    const items = Array.from({ length: 30 }, (_, index) => index);

    expect(launcherHomePreview("recent", items)).toHaveLength(18);
    expect(launcherHomePreview("pinned", items)).toHaveLength(9);
    expect(launcherHomePreview("marketplace", items)).toHaveLength(9);
    expect(launcherHomePreview("recent", items, true)).toBe(items);
    expect(launcherHomePreview("pinned", items, true)).toBe(items);
  });
});

describe("launcher home identity and history semantics", () => {
  it("keeps the first duplicate base item, then permits a live result override", () => {
    const first = { id: "shared", label: "built-in" };
    const duplicate = { id: "shared", label: "marketplace" };
    const live = { id: "shared", label: "live search" };

    expect(buildLauncherItemIndex([first, duplicate]).get("shared")).toBe(first);
    expect(buildLauncherItemIndex([first, duplicate], [live]).get("shared")).toBe(live);
  });

  it("excludes shell chrome without excluding real tools or applications", () => {
    expect(isLauncherRecentDestination("ihub.open-plugin-center")).toBe(false);
    expect(isLauncherRecentDestination("ihub.open-settings")).toBe(false);
    expect(isLauncherRecentDestination("system-command:ihub.open-settings")).toBe(false);
    expect(isLauncherRecentDestination("ihub.tool.json")).toBe(true);
    expect(isLauncherRecentDestination("application:C:/Program Files/App/app.exe")).toBe(true);
  });
});
