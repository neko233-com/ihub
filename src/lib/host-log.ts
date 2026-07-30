import type {
  HostLogEntry,
  HostLogLevel,
  HostLogSnapshot,
} from "./types";

const MAX_VISIBLE_ENTRIES = 1_000;
const MAX_VISIBLE_MESSAGE_CHARS = 2_049;
const MAX_VISIBLE_COMPONENT_CHARS = 48;

const browserFixtureEntries: HostLogEntry[] = [
  {
    timestamp: "2026-07-30T08:00:00.000Z",
    level: "info",
    component: "lifecycle",
    message: "浏览器安全预览已启动；这里不读取本机日志文件。",
  },
  {
    timestamp: "2026-07-30T08:00:00.120Z",
    level: "info",
    component: "hotkey",
    message: "启动快捷键注册预览完成（未调用系统快捷键 API）。",
  },
  {
    timestamp: "2026-07-30T08:00:00.260Z",
    level: "debug",
    component: "index",
    message: "本地索引预览就绪（文件内容、绝对路径与剪贴板均未载入）。",
  },
];

export function browserHostLogSnapshot(): HostLogSnapshot {
  return {
    generatedAt: "2026-07-30T08:00:00.300Z",
    entries: browserFixtureEntries.map((entry) => ({ ...entry })),
    truncated: false,
    totalBytes: 618,
    activeFileBytes: 618,
    maxFileBytes: 256 * 1024,
    maxFiles: 4,
    writeFailures: 0,
  };
}

export function emptyHostLogSnapshot(
  previous: HostLogSnapshot,
): HostLogSnapshot {
  return {
    ...previous,
    generatedAt: new Date().toISOString(),
    entries: [],
    truncated: false,
    totalBytes: 0,
    activeFileBytes: 0,
    writeFailures: 0,
    lastWriteError: undefined,
  };
}

export function canClearHostLog(
  snapshot: HostLogSnapshot | null,
  readError: string | null,
): boolean {
  return Boolean(
    readError
    || snapshot?.entries.length
    || snapshot?.totalBytes
    || snapshot?.activeFileBytes
    || snapshot?.writeFailures
    || snapshot?.lastWriteError,
  );
}

export function normalizeHostLogSnapshot(
  value: HostLogSnapshot,
): HostLogSnapshot {
  const entries = Array.isArray(value.entries)
    ? value.entries.slice(-MAX_VISIBLE_ENTRIES).flatMap(normalizeEntry)
    : [];
  return {
    generatedAt: safeTimestamp(value.generatedAt),
    entries,
    truncated: Boolean(value.truncated)
      || (Array.isArray(value.entries) && value.entries.length > MAX_VISIBLE_ENTRIES),
    totalBytes: safeNonNegativeInteger(value.totalBytes),
    activeFileBytes: safeNonNegativeInteger(value.activeFileBytes),
    maxFileBytes: safeNonNegativeInteger(value.maxFileBytes),
    maxFiles: Math.min(16, safeNonNegativeInteger(value.maxFiles)),
    writeFailures: safeNonNegativeInteger(value.writeFailures),
    lastWriteError: typeof value.lastWriteError === "string"
      ? cleanVisibleText(value.lastWriteError, MAX_VISIBLE_MESSAGE_CHARS)
      : undefined,
  };
}

export function formatHostLogForClipboard(
  snapshot: HostLogSnapshot,
): string {
  const normalized = normalizeHostLogSnapshot(snapshot);
  const header = [
    "# iHub bounded host diagnostics",
    `# generatedAt=${normalized.generatedAt}`,
    `# retained=${normalized.entries.length} truncated=${normalized.truncated}`,
    `# bytes=${normalized.totalBytes} maxFileBytes=${normalized.maxFileBytes} maxFiles=${normalized.maxFiles}`,
    `# writeFailures=${normalized.writeFailures}`,
  ];
  if (normalized.lastWriteError) {
    header.push(`# lastWriteError=${normalized.lastWriteError}`);
  }
  const lines = normalized.entries.map((entry) =>
    `${entry.timestamp} ${entry.level.toUpperCase().padEnd(5)} [${entry.component}] ${entry.message}`
  );
  return [...header, ...lines].join("\n");
}

export function formatHostLogBytes(bytes: number): string {
  const normalized = safeNonNegativeInteger(bytes);
  if (normalized < 1_024) {
    return `${normalized} B`;
  }
  if (normalized < 1_024 * 1_024) {
    return `${(normalized / 1_024).toFixed(1)} KiB`;
  }
  return `${(normalized / (1_024 * 1_024)).toFixed(1)} MiB`;
}

export function formatHostLogTimestamp(timestamp: string): string {
  const value = new Date(timestamp);
  if (Number.isNaN(value.getTime())) {
    return "--:--:--";
  }
  return value.toLocaleTimeString([], {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    hour12: false,
  });
}

function normalizeEntry(value: unknown): HostLogEntry[] {
  if (!value || typeof value !== "object") {
    return [];
  }
  const record = value as Partial<HostLogEntry>;
  const level = normalizeLevel(record.level);
  const component = typeof record.component === "string"
    ? cleanVisibleText(record.component, MAX_VISIBLE_COMPONENT_CHARS)
    : "host";
  const message = typeof record.message === "string"
    ? cleanVisibleText(record.message, MAX_VISIBLE_MESSAGE_CHARS)
    : "";
  if (!message) {
    return [];
  }
  return [{
    timestamp: safeTimestamp(record.timestamp),
    level,
    component: component || "host",
    message,
  }];
}

function normalizeLevel(level: unknown): HostLogLevel {
  return level === "debug"
      || level === "warn"
      || level === "error"
    ? level
    : "info";
}

function safeTimestamp(value: unknown): string {
  if (typeof value !== "string" || Number.isNaN(Date.parse(value))) {
    return new Date(0).toISOString();
  }
  return value;
}

function safeNonNegativeInteger(value: unknown): number {
  return typeof value === "number" && Number.isFinite(value)
    ? Math.max(0, Math.floor(value))
    : 0;
}

function cleanVisibleText(value: string, maxCharacters: number): string {
  let output = "";
  let count = 0;
  for (const character of value) {
    if (count >= maxCharacters) {
      break;
    }
    output += /[\u0000-\u001f\u007f]/.test(character) ? " " : character;
    count += 1;
  }
  return output;
}
