import { describe, expect, it } from "vitest";
import {
  clipboardHistoryKindLabel,
  clipboardHistoryRestoreLabel,
  formatClipboardHistoryBytes,
  isClipboardTextHistoryItem,
} from "./clipboard-history-v2";
import type { ClipboardHistoryItem } from "./types";

const textItem: ClipboardHistoryItem = {
  id: "text-1",
  kind: "text",
  text: "private note",
  capturedAt: "2026-01-01T00:00:00Z",
  pinned: false,
  files: [],
};

const imageItem: ClipboardHistoryItem = {
  id: "image-1",
  kind: "image",
  text: "",
  capturedAt: "2026-01-01T00:00:00Z",
  pinned: false,
  image: { width: 1280, height: 720, byteLength: 1_572_864 },
  files: [],
};

describe("clipboard history v2 renderer policy", () => {
  it("keeps launcher text matching limited to text history records", () => {
    expect(isClipboardTextHistoryItem(textItem)).toBe(true);
    expect(isClipboardTextHistoryItem(imageItem)).toBe(false);
  });

  it("labels explicit restore actions by format", () => {
    expect(clipboardHistoryKindLabel("files")).toBe("文件引用");
    expect(clipboardHistoryRestoreLabel("image")).toBe("还原图片");
    expect(formatClipboardHistoryBytes(1_572_864)).toBe("1.5 MB");
  });
});
