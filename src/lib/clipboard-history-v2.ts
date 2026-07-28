import type {
  ClipboardHistoryItem,
  ClipboardHistoryItemKind,
} from "./types";

/** Image/file records are never treated as searchable text. This keeps the
 * launcher result surface text-only while the Toolbox owns explicit restore
 * actions for richer clipboard formats. */
export function isClipboardTextHistoryItem(
  item: ClipboardHistoryItem,
): item is ClipboardHistoryItem & { kind: "text" } {
  return item.kind === "text";
}

export function clipboardHistoryKindLabel(kind: ClipboardHistoryItemKind) {
  switch (kind) {
    case "image":
      return "图片";
    case "files":
      return "文件引用";
    default:
      return "文本";
  }
}

export function clipboardHistoryRestoreLabel(kind: ClipboardHistoryItemKind) {
  switch (kind) {
    case "image":
      return "还原图片";
    case "files":
      return "复制文件引用";
    default:
      return "复制文本";
  }
}

export function formatClipboardHistoryBytes(bytes: number) {
  if (!Number.isFinite(bytes) || bytes <= 0) {
    return "0 KB";
  }
  if (bytes < 1024 * 1024) {
    return `${Math.max(1, Math.round(bytes / 1024))} KB`;
  }
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}
