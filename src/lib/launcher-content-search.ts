import type {
  ClipboardHistorySnapshot,
  SearchResult,
} from "./types";
import { isClipboardTextHistoryItem } from "./clipboard-history-v2";

/**
 * Kept in sync with ToolboxDrawer. Notes are deliberately local-only and are
 * read here rather than copied into a second persistence store.
 */
export const quickNotesStorageKey = "ihub.toolbox.quick-notes.v1";

export interface QuickNoteSearchItem {
  id: string;
  text: string;
  createdAt: number;
  updatedAt: number;
}

function isQuickNote(value: unknown): value is QuickNoteSearchItem {
  if (!value || typeof value !== "object") {
    return false;
  }
  const candidate = value as Partial<QuickNoteSearchItem>;
  return Boolean(
    typeof candidate.id === "string"
      && candidate.id
      && typeof candidate.text === "string"
      && candidate.text.trim()
      && typeof candidate.createdAt === "number"
      && Number.isFinite(candidate.createdAt)
      && typeof candidate.updatedAt === "number"
      && Number.isFinite(candidate.updatedAt),
  );
}

/** Reads only the user's existing local quick-note store; it never seeds data. */
export function readLauncherQuickNotes(): QuickNoteSearchItem[] {
  if (typeof window === "undefined") {
    return [];
  }

  try {
    const stored = window.localStorage.getItem(quickNotesStorageKey);
    const parsed: unknown = stored ? JSON.parse(stored) : [];
    if (!Array.isArray(parsed)) {
      return [];
    }
    return parsed
      .filter(isQuickNote)
      .sort((left, right) => right.updatedAt - left.updatedAt)
      .slice(0, 100);
  } catch {
    return [];
  }
}

function compactText(value: string) {
  return value.replace(/\s+/g, " ").trim();
}

function previewText(value: string, query: string, limit = 112) {
  const compact = compactText(value);
  const index = compact.toLocaleLowerCase().indexOf(query);
  if (compact.length <= limit) {
    return compact;
  }
  if (index <= 0 || index < Math.floor(limit / 2)) {
    return compact.slice(0, limit) + "…";
  }
  const start = Math.max(0, index - Math.floor(limit * 0.36));
  const end = Math.min(compact.length, start + limit);
  return (start ? "…" : "") + compact.slice(start, end) + (end < compact.length ? "…" : "");
}

function noteTitle(value: string) {
  return value.trim().split(/\r?\n/, 1)[0] || "未命名速记";
}

/**
 * Turns real, local-only text into normal launcher results. Clipboard records
 * are intentionally ignored unless the native host explicitly reports history
 * as enabled; browser preview callers pass null and receive no fake records.
 */
export function findLauncherContentResults(
  query: string,
  quickNotes: readonly QuickNoteSearchItem[],
  clipboardHistory: ClipboardHistorySnapshot | null,
): SearchResult[] {
  const normalized = query.trim().toLocaleLowerCase();
  if (!normalized) {
    return [];
  }

  const notes = quickNotes
    .filter((note) => note.text.toLocaleLowerCase().includes(normalized))
    .slice(0, 6)
    .map((note, index) => ({
      id: "quick-note:" + note.id,
      name: noteTitle(note.text),
      kind: "command" as const,
      score: 970 - index,
      metadata: "速记 · " + previewText(note.text, normalized),
      commandId: "ihub.tool.quick-note",
    }));

  if (!clipboardHistory?.enabled) {
    return notes;
  }

  const clipboard = clipboardHistory.items
    .filter(isClipboardTextHistoryItem)
    .filter((item) => item.text.toLocaleLowerCase().includes(normalized))
    .slice(0, 6)
    .map((item, index) => ({
      id: "clipboard-history:" + item.id,
      name: previewText(item.text, normalized),
      kind: "command" as const,
      score: 950 - index,
      metadata: "剪贴板历史" + (item.pinned ? " · 已固定" : ""),
      commandId: "ihub.tool.clipboard-history",
    }));

  return [...notes, ...clipboard];
}
