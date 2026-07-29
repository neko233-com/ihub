import { describe, expect, it } from "vitest";
import {
  MAX_NATIVE_ICON_DATA_URL_BYTES,
  safeNativeIconSrc,
  sanitizeSystemIconMap,
  systemIconRequestChunks,
} from "./native-icons";

const onePixelPng = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZrG8AAAAASUVORK5CYII=";

describe("native icon IPC boundaries", () => {
  it("accepts only bounded PNG data URLs with well-formed base64", () => {
    expect(safeNativeIconSrc(onePixelPng)).toBe(onePixelPng);
    expect(safeNativeIconSrc("https://example.com/icon.png")).toBeUndefined();
    expect(safeNativeIconSrc("data:image/svg+xml;base64,PHN2Zz4=")).toBeUndefined();
    expect(safeNativeIconSrc("data:image/png;base64,not base64")).toBeUndefined();
    expect(safeNativeIconSrc(`data:image/png;base64,${"A".repeat(MAX_NATIVE_ICON_DATA_URL_BYTES)}`))
      .toBeUndefined();
  });

  it("drops stale, unknown, and malformed response entries", () => {
    expect(sanitizeSystemIconMap({
      current: onePixelPng,
      stale: onePixelPng,
      malformed: "data:image/png;base64,!",
    }, new Set(["current", "malformed"]))).toEqual({ current: onePixelPng });
  });

  it("deduplicates and caps every host request to twelve targets", () => {
    const chunks = systemIconRequestChunks(
      Array.from({ length: 13 }, (_, index) => `search-${index}`),
      ["search-1", "shortcut-1"],
    );
    expect(chunks).toHaveLength(2);
    expect(chunks[0].searchResultIds).toHaveLength(12);
    expect(chunks[0].launcherShortcutIds).toHaveLength(0);
    expect(chunks[1]).toEqual({
      searchResultIds: ["search-12"],
      launcherShortcutIds: ["shortcut-1"],
    });
  });
});
