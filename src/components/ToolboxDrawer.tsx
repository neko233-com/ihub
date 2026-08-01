import { AnimatePresence, motion } from "motion/react";
import {
  ArrowRight,
  Binary,
  BookOpenText,
  Braces,
  Calculator,
  Camera,
  Check,
  Clipboard,
  CircleAlert,
  Cloud,
  Clock3,
  Code2,
  Copy,
  Crop,
  Download,
  Files,
  FolderSearch,
  LoaderCircle,
  NotebookPen,
  Palette,
  Pause,
  Pin,
  PinOff,
  Play,
  Plus,
  QrCode,
  RefreshCw,
  Search,
  Trash2,
  Video,
  X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ColorWorkbench } from "./ColorWorkbench";
import { LocalSearchWorkspace } from "./LocalSearchWorkspace";
import { JsonEditorWorkspace } from "./JsonEditorWorkspace";
import { MarkdownWorkbench } from "./MarkdownWorkbench";
import { RegionCaptureEditor } from "./RegionCaptureEditor";
import {
  clipboardHistoryKindLabel,
  clipboardHistoryRestoreLabel,
  formatClipboardHistoryBytes,
} from "../lib/clipboard-history-v2";
import { evaluateCalculatorExpression } from "../lib/calculator";
import { command, isDesktop } from "../lib/desktop";
import { displayLocalPath } from "../lib/path-display";
import { decodeQrImageFile } from "../lib/qr-image-decode";
import { activeRecordingElapsedMs, remainingActiveRecordingMs } from "../lib/recording-timing";
import {
  createRegionCaptureDemoSource,
  validateRegionCaptureSize,
  type CroppedCapture,
  type RegionCaptureSource,
} from "../lib/region-capture";
import {
  createTimeSnapshot,
  parseTimeInput,
  type TimeInputKind,
} from "../lib/time-tools";
import {
  formatWebDavBytes,
  isWebDavUrlWithinRoot,
  parseWebDavDirectoryXml,
  parseWebDavEndpoint,
  type WebDavDirectoryResponse,
  type WebDavDownloadResult,
  type WebDavEntry,
  type WebDavUploadResult,
} from "../lib/webdav";
import { cloudDriveProviders } from "../lib/cloud-drive-providers";
import {
  buildCloudDriveDisconnectRequest,
  buildCloudProfileForgetRequest,
  buildWebDavConnectRequest,
  buildWebDavDownloadRequest,
  buildWebDavListRequest,
  buildWebDavSavedConnectRequest,
  buildWebDavUploadRequest,
  type CloudProfileView,
  type WebDavConnectResult,
} from "../lib/cloud-drive-session";
import type {
  ClipboardHistoryItem,
  ClipboardHistoryRestoreResult,
  ClipboardHistorySnapshot,
  ClipboardImage,
  IndexStatus,
  PluginInfo,
  SearchResult,
  SelectedDirectoryGrant,
} from "../lib/types";

export type ToolboxTab =
  | "search"
  | "color"
  | "screenshot"
  | "clipboard"
  | "json"
  | "markdown"
  | "note"
  | "convert"
  | "calculator"
  | "time"
  | "qrcode"
  | "cloud"
  | "record"
  | "rename"
  | "developer";

/** A transient launcher handoff. It stays in renderer memory and is applied
 * once to the matching built-in tool; no text or path is persisted merely by
 * opening a context action. */
export interface ToolboxLaunchContext {
  requestId: number;
  jsonInput?: string;
  renameDirectory?: string;
  renameDirectoryOpenId?: string;
  calculatorInput?: string;
  timeInput?: string;
}

interface ToolboxDrawerProps {
  activeTab: ToolboxTab;
  indexStatus: IndexStatus;
  isRefreshingIndex: boolean;
  onClose: () => void;
  onOpenSearchResult: (result: SearchResult) => Promise<void> | void;
  onRefreshIndex: () => void;
  onSetIndexRoots: (
    roots: string[],
    directoryOpenIds: string[],
  ) => Promise<void> | void;
  onStartWindowDrag?: () => void;
  onTabChange: (tab: ToolboxTab) => void;
  onToast: (message: string) => void;
  onPluginsChanged: (plugins: PluginInfo[]) => void;
  onRecordingPhaseChange: (phase: RecordingPhase) => void;
  /** A user-pasted image handed off from the launcher without persistence. */
  pastedImage?: {
    blob: Blob;
    name: string;
    type: string;
  } | null;
  onPastedImageConsumed?: () => void;
  launchContext?: ToolboxLaunchContext | null;
  open: boolean;
  plugins: PluginInfo[];
}

interface BatchRenameItem {
  from: string;
  to: string;
}

interface BatchRenamePreview {
  directory: string;
  items: BatchRenameItem[];
  canApply: boolean;
  errors: string[];
}

interface BatchRenameResult {
  renamed: number;
}

interface PluginProjectResult {
  projectPath: string;
  pluginId: string;
  nextSteps: string[];
  openId: string;
}

interface QuickNote {
  id: string;
  text: string;
  createdAt: number;
  updatedAt: number;
}

type ConversionBase = 2 | 8 | 10 | 16;
type TextEncoding = "hex" | "base64";
type TextConversionDirection = "encode" | "decode";
type DirectoryPickerTarget = "rename" | "project" | "local-plugin";

interface NumberConversionResult {
  valid: boolean;
  values?: Array<{
    base: ConversionBase;
    label: string;
    value: string;
  }>;
  error?: string;
}

interface TextConversionResult {
  valid: boolean;
  value?: string;
  error?: string;
}

interface EyeDropperResult {
  sRGBHex: string;
}

interface EyeDropperInstance {
  open(): Promise<EyeDropperResult>;
}

interface EyeDropperConstructor {
  new (): EyeDropperInstance;
}

/** A bounded, one-shot PNG returned by the trusted iHub shell. This is only
 * used by the built-in UI after a direct click; plugins do not receive this
 * native capture channel. */
interface NativeScreenshot {
  dataUrl: string;
  name: string;
  mimeType: "image/png";
  width: number;
  height: number;
  displayIndex: number;
}

const quickNotesStorageKey = "ihub.toolbox.quick-notes.v1";
/**
 * MediaRecorder buffers every chunk in the WebView until the final WebM is
 * downloaded. Keep the built-in recorder intentionally bounded so a forgotten
 * capture cannot exhaust renderer memory. Longer or transcoded recordings are
 * a job for a native recording plugin.
 */
const maxScreenRecordingDurationMs = 30 * 60 * 1_000;
const maxScreenRecordingBytes = 512 * 1024 * 1024;

type RecordingStopReason =
  | "manual"
  | "duration-limit"
  | "size-limit"
  | "source-ended"
  | "drawer-closed"
  | "error";

export type RecordingPhase =
  | "idle"
  | "starting"
  | "recording"
  | "paused"
  | "stopping";

const numberBases: Array<{ base: ConversionBase; label: string }> = [
  { base: 2, label: "BIN" },
  { base: 8, label: "OCT" },
  { base: 10, label: "DEC" },
  { base: 16, label: "HEX" },
];

const tabs: Array<{
  id: ToolboxTab;
  label: string;
  icon: typeof Search;
}> = [
  { id: "search", label: "本地搜索", icon: Search },
  { id: "color", label: "颜色", icon: Palette },
  { id: "screenshot", label: "截图", icon: Camera },
  { id: "clipboard", label: "剪贴板", icon: Clipboard },
  { id: "json", label: "JSON", icon: Braces },
  { id: "markdown", label: "Markdown", icon: BookOpenText },
  { id: "note", label: "便签", icon: NotebookPen },
  { id: "convert", label: "转换", icon: Binary },
  { id: "calculator", label: "计算器", icon: Calculator },
  { id: "time", label: "时间", icon: Clock3 },
  { id: "qrcode", label: "二维码", icon: QrCode },
  { id: "cloud", label: "云盘", icon: Cloud },
  { id: "record", label: "录屏", icon: Video },
  { id: "rename", label: "重命名", icon: Files },
  { id: "developer", label: "开发者", icon: Code2 },
];

const commonTimeZones = [
  "UTC",
  "Asia/Shanghai",
  "Asia/Tokyo",
  "Asia/Singapore",
  "Europe/London",
  "Europe/Berlin",
  "America/New_York",
  "America/Los_Angeles",
] as const;

function resolvedSystemTimeZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
}

function timeInputKindLabel(kind: TimeInputKind): string {
  switch (kind) {
    case "unix-seconds":
      return "已识别为 Unix 秒";
    case "unix-milliseconds":
      return "已识别为 Unix 毫秒";
    case "iso-date":
      return "已识别为 ISO 8601";
    case "local-date":
      return "已按本机时区解析日期";
  }
}

function revokeScreenshotObjectUrl(value: string | null): void {
  // Native captures are returned as bounded data URLs. Revoking only blob
  // URLs makes the lifecycle explicit and avoids treating a data URL as an
  // owned browser object URL.
  if (value?.startsWith("blob:")) {
    URL.revokeObjectURL(value);
  }
}

function isPluginId(value: string) {
  return /^[a-z][a-z0-9]*(?:-[a-z0-9]+)*$/.test(value);
}

function formatElapsed(milliseconds: number) {
  const totalSeconds = Math.floor(milliseconds / 1000);
  const minutes = Math.floor(totalSeconds / 60).toString().padStart(2, "0");
  const seconds = (totalSeconds % 60).toString().padStart(2, "0");
  return `${minutes}:${seconds}`;
}

function formatByteSize(bytes: number) {
  if (bytes < 1024 * 1024) {
    return `${Math.max(0, Math.round(bytes / 1024))} KB`;
  }
  return `${(bytes / (1024 * 1024)).toFixed(bytes >= 100 * 1024 * 1024 ? 0 : 1)} MB`;
}

function readQuickNotes(): QuickNote[] {
  try {
    const stored = window.localStorage.getItem(quickNotesStorageKey);
    if (!stored) {
      return [];
    }

    const parsed: unknown = JSON.parse(stored);
    if (!Array.isArray(parsed)) {
      return [];
    }

    return parsed
      .flatMap((entry): QuickNote[] => {
        if (!entry || typeof entry !== "object") {
          return [];
        }
        const candidate = entry as Partial<QuickNote>;
        if (
          typeof candidate.id !== "string" ||
          !candidate.id ||
          typeof candidate.text !== "string" ||
          !candidate.text.trim() ||
          typeof candidate.createdAt !== "number" ||
          !Number.isFinite(candidate.createdAt) ||
          typeof candidate.updatedAt !== "number" ||
          !Number.isFinite(candidate.updatedAt)
        ) {
          return [];
        }
        return [
          {
            id: candidate.id,
            text: candidate.text,
            createdAt: candidate.createdAt,
            updatedAt: candidate.updatedAt,
          },
        ];
      })
      .sort((left, right) => right.updatedAt - left.updatedAt)
      .slice(0, 100);
  } catch {
    return [];
  }
}

function createQuickNoteId() {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

function noteTitle(text: string) {
  return text.trim().split(/\r?\n/, 1)[0] || "未命名便签";
}

function notePreview(text: string) {
  const compact = text.replace(/\s+/g, " ").trim();
  return compact.length > 96 ? `${compact.slice(0, 96)}…` : compact;
}

function formatNoteTime(timestamp: number) {
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(timestamp);
}

function formatClipboardTime(timestamp: string) {
  const date = new Date(timestamp);
  if (Number.isNaN(date.getTime())) {
    return "刚刚";
  }
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(date);
}

function saveBlob(blob: Blob, filename: string) {
  const url = URL.createObjectURL(blob);
  const anchor = document.createElement("a");
  anchor.href = url;
  anchor.download = filename;
  anchor.click();
  window.setTimeout(() => URL.revokeObjectURL(url), 1_000);
}

function eyeDropperConstructor(): EyeDropperConstructor | undefined {
  if (typeof window === "undefined") {
    return undefined;
  }
  return (window as unknown as { EyeDropper?: EyeDropperConstructor }).EyeDropper;
}

function parseInteger(input: string, base: ConversionBase) {
  const trimmed = input.trim().replaceAll("_", "");
  if (!trimmed) {
    throw new Error("请输入一个整数。");
  }

  const hasSign = trimmed.startsWith("-") || trimmed.startsWith("+");
  const sign = trimmed.startsWith("-") ? -1n : 1n;
  let digits = hasSign ? trimmed.slice(1) : trimmed;
  const explicitPrefix = digits.slice(0, 2).toLowerCase();
  const prefixedBase: ConversionBase | null = explicitPrefix === "0b"
    ? 2
    : explicitPrefix === "0o"
      ? 8
      : explicitPrefix === "0x"
        ? 16
        : null;
  const inferredBase = prefixedBase ?? base;
  if (prefixedBase) {
    digits = digits.slice(2);
  }

  const allowedDigits = inferredBase === 2 ? /^[01]+$/ : inferredBase === 8 ? /^[0-7]+$/ : inferredBase === 10 ? /^\d+$/ : /^[\da-f]+$/i;
  if (!allowedDigits.test(digits)) {
    throw new Error(`${inferredBase} 进制输入包含无效字符。`);
  }

  const prefix = inferredBase === 2 ? "0b" : inferredBase === 8 ? "0o" : inferredBase === 16 ? "0x" : "";
  const absolute = inferredBase === 10 ? BigInt(digits) : BigInt(`${prefix}${digits}`);
  return sign * absolute;
}

function formatInteger(value: bigint, base: ConversionBase) {
  const prefix = base === 2 ? "0b" : base === 8 ? "0o" : base === 16 ? "0x" : "";
  const negative = value < 0n;
  const digits = (negative ? -value : value).toString(base).toUpperCase();
  return `${negative ? "-" : ""}${prefix}${digits}`;
}

function parseBoundedWholeNumber(value: string, maximum: number): number | null {
  const normalized = value.trim();
  if (!/^\d+$/.test(normalized)) {
    return null;
  }
  const parsed = Number(normalized);
  return Number.isSafeInteger(parsed) && parsed <= maximum ? parsed : null;
}

function convertNumber(input: string, base: ConversionBase): NumberConversionResult {
  if (!input.trim()) {
    return { valid: true, values: [] };
  }

  try {
    const value = parseInteger(input, base);
    return {
      valid: true,
      values: numberBases.map(({ base: targetBase, label }) => ({
        base: targetBase,
        label,
        value: formatInteger(value, targetBase),
      })),
    };
  } catch (error) {
    return {
      valid: false,
      error: error instanceof Error ? error.message : "无法转换该整数。",
    };
  }
}

function bytesToHex(bytes: Uint8Array) {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0").toUpperCase()).join(" ");
}

function hexToBytes(input: string) {
  const normalized = input.trim().replace(/0x/gi, "").replace(/[\s,:-]/g, "");
  if (!normalized) {
    return new Uint8Array();
  }
  if (!/^[\da-f]+$/i.test(normalized) || normalized.length % 2 !== 0) {
    throw new Error("请输入偶数位的十六进制字节，例如 E4 B8 AD。");
  }
  return Uint8Array.from(
    normalized.match(/.{2}/g) ?? [],
    (pair) => Number.parseInt(pair, 16),
  );
}

function encodeBase64(bytes: Uint8Array) {
  let binary = "";
  bytes.forEach((byte) => {
    binary += String.fromCharCode(byte);
  });
  return btoa(binary);
}

function decodeBase64(input: string) {
  try {
    const binary = atob(input.trim().replace(/\s/g, ""));
    return Uint8Array.from(binary, (character) => character.charCodeAt(0));
  } catch {
    throw new Error("Base64 内容无效。请检查填充和字符。");
  }
}

function convertText(
  input: string,
  direction: TextConversionDirection,
  encoding: TextEncoding,
): TextConversionResult {
  try {
    if (direction === "encode") {
      const bytes = new TextEncoder().encode(input);
      return {
        valid: true,
        value: encoding === "hex" ? bytesToHex(bytes) : encodeBase64(bytes),
      };
    }

    const bytes = encoding === "hex" ? hexToBytes(input) : decodeBase64(input);
    return {
      valid: true,
      value: new TextDecoder("utf-8", { fatal: true }).decode(bytes),
    };
  } catch (error) {
    return {
      valid: false,
      error: error instanceof Error ? error.message : "无法转换文本。",
    };
  }
}

interface CalculatorHistoryItem {
  id: string;
  expression: string;
  result: string;
  createdAt: number;
}

const calculatorHistoryStorageKey = "ihub.toolbox.calculator-history.v1";
const calculatorKeys = [
  "(", ")", "%", "^", "⌫",
  "7", "8", "9", "/", "*",
  "4", "5", "6", "-", "+",
  "1", "2", "3", ".", "=",
  "0",
] as const;

function calculatorHistoryId() {
  if (typeof crypto !== "undefined" && typeof crypto.randomUUID === "function") {
    return crypto.randomUUID();
  }
  return `calculation-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

function readCalculatorHistory(): CalculatorHistoryItem[] {
  try {
    const stored = window.localStorage.getItem(calculatorHistoryStorageKey);
    if (!stored) {
      return [];
    }
    const parsed: unknown = JSON.parse(stored);
    if (!Array.isArray(parsed)) {
      return [];
    }
    return parsed
      .flatMap((entry): CalculatorHistoryItem[] => {
        if (!entry || typeof entry !== "object") {
          return [];
        }
        const candidate = entry as Partial<CalculatorHistoryItem>;
        if (
          typeof candidate.id !== "string"
          || typeof candidate.expression !== "string"
          || !candidate.expression.trim()
          || typeof candidate.result !== "string"
          || !candidate.result
          || typeof candidate.createdAt !== "number"
          || !Number.isFinite(candidate.createdAt)
        ) {
          return [];
        }
        return [{
          id: candidate.id,
          expression: candidate.expression,
          result: candidate.result,
          createdAt: candidate.createdAt,
        }];
      })
      .sort((left, right) => right.createdAt - left.createdAt)
      .slice(0, 24);
  } catch {
    return [];
  }
}

/**
 * `getDisplayMedia` opens a browser/system picker that temporarily takes focus
 * away from iHub. On desktop, guard only that pending picker with a native
 * lease; release in `finally` even when the user cancels or the picker errors.
 * The native lease has its own deadline in case this renderer disappears.
 */
async function getDisplayMediaWithFocusLease(
  constraints: Parameters<MediaDevices["getDisplayMedia"]>[0],
): Promise<MediaStream> {
  if (!isDesktop()) {
    return navigator.mediaDevices.getDisplayMedia(constraints);
  }

  // Chromium requires getDisplayMedia() to be called from the original user
  // gesture. Sending the native request first is synchronous from this
  // renderer's perspective, but awaiting its IPC response here would cross an
  // event turn and can make the browser reject the picker as non-user-initiated.
  // Keep the request in flight, open the chooser immediately, then wait only
  // while cleaning up. A failed lease is a graceful fallback: capture remains
  // available, while the native host still restores its usual behavior.
  const leaseRequest = command<string>("acquire_capture_focus_lease").then(
    (leaseId) => ({ leaseId }),
    () => ({ leaseId: null }),
  );
  let released = false;
  const releaseLease = async () => {
    if (released) {
      return;
    }
    released = true;
    const { leaseId } = await leaseRequest;
    if (!leaseId) {
      return;
    }
    try {
      await command<void>("release_capture_focus_lease", { leaseId });
    } catch {
      // The native deadline still restores normal auto-hide behavior if the
      // release cannot reach the host (for example during app shutdown).
    }
  };

  try {
    return await navigator.mediaDevices.getDisplayMedia(constraints);
  } finally {
    await releaseLease();
  }
}

export function ToolboxDrawer({
  activeTab,
  indexStatus,
  isRefreshingIndex,
  onClose,
  onOpenSearchResult,
  onRefreshIndex,
  onSetIndexRoots,
  onStartWindowDrag,
  onTabChange,
  onToast,
  onPluginsChanged,
  onRecordingPhaseChange,
  pastedImage,
  onPastedImageConsumed,
  launchContext,
  open,
  plugins,
}: ToolboxDrawerProps) {
  const recorderRef = useRef<MediaRecorder | null>(null);
  const recordingStreamRef = useRef<MediaStream | null>(null);
  const recordingChunksRef = useRef<Blob[]>([]);
  const recordingBytesRef = useRef(0);
  const recordingLimitTimerRef = useRef<number | null>(null);
  const recordingStopReasonRef = useRef<RecordingStopReason | null>(null);
  const recordingPhaseRef = useRef<RecordingPhase>("idle");
  const recordingStartAttemptRef = useRef(0);
  const recordingActiveStartedAtRef = useRef<number | null>(null);
  const recordingActiveElapsedMsRef = useRef(0);
  const mountedRef = useRef(true);
  const openRef = useRef(open);
  const screenshotPreviewUrlRef = useRef<string | null>(null);
  const regionCaptureSourceRef = useRef<RegionCaptureSource | null>(null);
  const handledPastedImageRef = useRef<Blob | null>(null);
  const handledLaunchContextRequestRef = useRef<number | null>(null);
  const calculatorInputRef = useRef<HTMLInputElement | null>(null);
  const timeInputRef = useRef<HTMLInputElement | null>(null);
  const qrDecodeInputRef = useRef<HTMLInputElement | null>(null);
  const webDavRequestIdRef = useRef(0);
  const [color, setColor] = useState("#76e8d6");
  const [isCapturingScreenshot, setIsCapturingScreenshot] = useState(false);
  const [regionCaptureSource, setRegionCaptureSource] = useState<RegionCaptureSource | null>(null);
  const [screenshotPreviewUrl, setScreenshotPreviewUrl] = useState<string | null>(null);
  const [screenshotPreviewDescription, setScreenshotPreviewDescription] = useState("最新截图会显示在这里。");
  const [screenshotDownloadName, setScreenshotDownloadName] = useState("ihub-capture.png");
  const [clipboardHistory, setClipboardHistory] = useState<ClipboardHistorySnapshot | null>(null);
  /** Image pixels enter renderer memory only after its matching Preview action. */
  const [clipboardImagePreview, setClipboardImagePreview] = useState<{
    id: string;
    image: ClipboardImage;
  } | null>(null);
  const [isLoadingClipboardHistory, setIsLoadingClipboardHistory] = useState(false);
  const [clipboardActionId, setClipboardActionId] = useState<string | null>(null);
  const [jsonInput, setJsonInput] = useState('{\n  "name": "iHub",\n  "fast": true\n}');
  const [quickNotes, setQuickNotes] = useState<QuickNote[]>(readQuickNotes);
  const [quickNoteDraft, setQuickNoteDraft] = useState("");
  const [quickNoteQuery, setQuickNoteQuery] = useState("");
  const [conversionInput, setConversionInput] = useState("2026");
  const [conversionBase, setConversionBase] = useState<ConversionBase>(10);
  const [textConversionInput, setTextConversionInput] = useState("你好，iHub");
  const [textEncoding, setTextEncoding] = useState<TextEncoding>("hex");
  const [textConversionDirection, setTextConversionDirection] = useState<TextConversionDirection>("encode");
  const [calculatorInput, setCalculatorInput] = useState("(512 + 256) / 3");
  const [calculatorHistory, setCalculatorHistory] = useState<CalculatorHistoryItem[]>(readCalculatorHistory);
  const [timeInput, setTimeInput] = useState(() => Date.now().toString());
  const [currentTimeMilliseconds, setCurrentTimeMilliseconds] = useState(() => Date.now());
  const [localTimeZone] = useState(resolvedSystemTimeZone);
  const [selectedTimeZone, setSelectedTimeZone] = useState("Asia/Shanghai");
  const [qrInput, setQrInput] = useState("https://ihub.local");
  const [qrPreviewUrl, setQrPreviewUrl] = useState<string | null>(null);
  const [qrError, setQrError] = useState<string | null>(null);
  const [isGeneratingQr, setIsGeneratingQr] = useState(false);
  const [qrDecodeValue, setQrDecodeValue] = useState<string | null>(null);
  const [qrDecodeFileName, setQrDecodeFileName] = useState<string | null>(null);
  const [qrDecodeError, setQrDecodeError] = useState<string | null>(null);
  const [isDecodingQr, setIsDecodingQr] = useState(false);
  const [webDavEndpoint, setWebDavEndpoint] = useState("");
  const [webDavUsername, setWebDavUsername] = useState("");
  const [webDavPassword, setWebDavPassword] = useState("");
  const [webDavRemember, setWebDavRemember] = useState(false);
  const [webDavProfileLabel, setWebDavProfileLabel] = useState("");
  const [cloudProfiles, setCloudProfiles] = useState<CloudProfileView[]>([]);
  const [isLoadingCloudProfiles, setIsLoadingCloudProfiles] = useState(false);
  const [connectingCloudProfileId, setConnectingCloudProfileId] = useState<string | null>(null);
  const [forgettingCloudProfileId, setForgettingCloudProfileId] = useState<string | null>(null);
  const [webDavConnectionId, setWebDavConnectionId] = useState<string | null>(null);
  const webDavConnectionIdRef = useRef<string | null>(null);
  const [webDavConnectedProfileId, setWebDavConnectedProfileId] = useState<string | null>(null);
  const [webDavConnectedRoot, setWebDavConnectedRoot] = useState<string | null>(null);
  const [webDavDirectory, setWebDavDirectory] = useState<string | null>(null);
  const [webDavEntries, setWebDavEntries] = useState<ReturnType<typeof parseWebDavDirectoryXml>>([]);
  const [webDavError, setWebDavError] = useState<string | null>(null);
  const [isLoadingWebDav, setIsLoadingWebDav] = useState(false);
  const [webDavDownloadingHref, setWebDavDownloadingHref] = useState<string | null>(null);
  const [isUploadingWebDav, setIsUploadingWebDav] = useState(false);
  const [includeSystemAudio, setIncludeSystemAudio] = useState(true);
  const [recordingPhase, setRecordingPhase] = useState<RecordingPhase>("idle");
  const [recordingElapsed, setRecordingElapsed] = useState(0);
  const [recordingBytes, setRecordingBytes] = useState(0);
  const [renameDirectory, setRenameDirectory] = useState("");
  const [renameDirectoryOpenId, setRenameDirectoryOpenId] = useState<string | null>(null);
  const [renameFind, setRenameFind] = useState("");
  const [renameReplace, setRenameReplace] = useState("");
  const [renameUseRegex, setRenameUseRegex] = useState(false);
  const [renameSequenceStart, setRenameSequenceStart] = useState("1");
  const [renameSequencePadding, setRenameSequencePadding] = useState("3");
  const [renamePreview, setRenamePreview] = useState<BatchRenamePreview | null>(null);
  const [isPreviewingRename, setIsPreviewingRename] = useState(false);
  const [isApplyingRename, setIsApplyingRename] = useState(false);
  const [projectParentDirectory, setProjectParentDirectory] = useState("");
  const [projectParentDirectoryOpenId, setProjectParentDirectoryOpenId] =
    useState<string | null>(null);
  const [projectId, setProjectId] = useState("ihub-plugin-my-feature");
  const [projectResult, setProjectResult] = useState<PluginProjectResult | null>(null);
  const [isCreatingProject, setIsCreatingProject] = useState(false);
  const [isOpeningProject, setIsOpeningProject] = useState(false);
  const [localPluginDirectory, setLocalPluginDirectory] = useState("");
  const [localPluginDirectoryOpenId, setLocalPluginDirectoryOpenId] =
    useState<string | null>(null);
  const [isLinkingLocalPlugin, setIsLinkingLocalPlugin] = useState(false);
  const [unlinkingPluginId, setUnlinkingPluginId] = useState<string | null>(null);
  const [directoryPickerTarget, setDirectoryPickerTarget] = useState<DirectoryPickerTarget | null>(null);

  const replaceScreenshotPreview = useCallback((previewUrl: string) => {
    revokeScreenshotObjectUrl(screenshotPreviewUrlRef.current);
    screenshotPreviewUrlRef.current = previewUrl;
    setScreenshotPreviewUrl(previewUrl);
  }, []);

  const replaceRegionCaptureSource = useCallback((source: RegionCaptureSource | null) => {
    const previous = regionCaptureSourceRef.current;
    if (previous?.revokeOnClose) {
      revokeScreenshotObjectUrl(previous.url);
    }
    regionCaptureSourceRef.current = source;
    setRegionCaptureSource(source);
  }, []);

  const filteredQuickNotes = useMemo(() => {
    const query = quickNoteQuery.trim().toLocaleLowerCase();
    if (!query) {
      return quickNotes;
    }
    return quickNotes.filter((note) => note.text.toLocaleLowerCase().includes(query));
  }, [quickNoteQuery, quickNotes]);
  const numberConversion = useMemo(
    () => convertNumber(conversionInput, conversionBase),
    [conversionBase, conversionInput],
  );
  const textConversion = useMemo(
    () => convertText(textConversionInput, textConversionDirection, textEncoding),
    [textConversionDirection, textConversionInput, textEncoding],
  );
  const calculatorEvaluation = useMemo(
    () => evaluateCalculatorExpression(calculatorInput),
    [calculatorInput],
  );
  const parsedTime = useMemo(() => parseTimeInput(timeInput), [timeInput]);
  const timeSnapshot = useMemo(
    () => parsedTime.ok
      ? createTimeSnapshot(parsedTime.epochMilliseconds, {
          localTimeZone,
          selectedTimeZone,
        })
      : null,
    [localTimeZone, parsedTime, selectedTimeZone],
  );
  const currentTimeSnapshot = useMemo(
    () => createTimeSnapshot(currentTimeMilliseconds, {
      localTimeZone,
      selectedTimeZone: localTimeZone,
    }),
    [currentTimeMilliseconds, localTimeZone],
  );
  const webDavLocation = useMemo(() => {
    if (!webDavConnectedRoot || !webDavDirectory) {
      return null;
    }
    try {
      const root = parseWebDavEndpoint(webDavConnectedRoot);
      const directory = new URL(webDavDirectory);
      if (!isWebDavUrlWithinRoot(root, directory)) {
        return null;
      }
      const parent = new URL("../", directory);
      return {
        canGoUp: isWebDavUrlWithinRoot(root, parent) && parent.href !== directory.href,
        path: directory.pathname,
      };
    } catch {
      return null;
    }
  }, [webDavConnectedRoot, webDavDirectory]);
  const isStartingRecording = recordingPhase === "starting";
  const isRecording = recordingPhase === "recording" || recordingPhase === "paused";
  const isRecordingPaused = recordingPhase === "paused";
  const isStoppingRecording = recordingPhase === "stopping";
  const isRecordingSessionActive = recordingPhase !== "idle";

  const applyClipboardHistorySnapshot = useCallback((snapshot: ClipboardHistorySnapshot) => {
    setClipboardHistory(snapshot);
    setClipboardImagePreview((current) => current
      && snapshot.enabled
      && snapshot.items.some((item) => item.id === current.id && item.kind === "image")
      ? current
      : null);
  }, []);

  openRef.current = open;

  const transitionRecordingPhase = (nextPhase: RecordingPhase) => {
    if (recordingPhaseRef.current === nextPhase) {
      return;
    }
    recordingPhaseRef.current = nextPhase;
    if (!mountedRef.current) {
      return;
    }
    setRecordingPhase(nextPhase);
    onRecordingPhaseChange(nextPhase);
  };

  const clearRecordingLimitTimer = () => {
    if (recordingLimitTimerRef.current !== null) {
      window.clearTimeout(recordingLimitTimerRef.current);
      recordingLimitTimerRef.current = null;
    }
  };

  const stopTracks = () => {
    recordingStreamRef.current?.getTracks().forEach((track) => track.stop());
    recordingStreamRef.current = null;
  };

  const freezeActiveRecordingElapsed = () => {
    const elapsed = activeRecordingElapsedMs(
      recordingActiveElapsedMsRef.current,
      recordingActiveStartedAtRef.current,
      performance.now(),
    );
    recordingActiveElapsedMsRef.current = elapsed;
    recordingActiveStartedAtRef.current = null;
    if (mountedRef.current) {
      setRecordingElapsed(elapsed);
    }
    return elapsed;
  };

  const resetRecordingState = () => {
    clearRecordingLimitTimer();
    recordingChunksRef.current = [];
    recordingBytesRef.current = 0;
    recordingStopReasonRef.current = null;
    recordingActiveElapsedMsRef.current = 0;
    recordingActiveStartedAtRef.current = null;
    if (mountedRef.current) {
      setRecordingBytes(0);
      setRecordingElapsed(0);
    }
    transitionRecordingPhase("idle");
  };

  const cancelPendingRecordingStart = () => {
    if (recordingPhaseRef.current !== "starting") {
      return;
    }
    recordingStartAttemptRef.current += 1;
    transitionRecordingPhase("idle");
  };

  const stopScreenRecording = (reason: RecordingStopReason = "manual") => {
    const currentPhase = recordingPhaseRef.current;
    if (currentPhase === "idle") {
      return;
    }
    if (currentPhase === "starting") {
      cancelPendingRecordingStart();
      return;
    }
    if (currentPhase === "stopping" || recordingStopReasonRef.current) {
      return;
    }

    if (currentPhase === "recording") {
      freezeActiveRecordingElapsed();
    }
    recordingStopReasonRef.current = reason;
    clearRecordingLimitTimer();
    transitionRecordingPhase("stopping");

    const recorder = recorderRef.current;
    if (recorder) {
      if (recorder.state !== "inactive") {
        try {
          recorder.stop();
        } catch {
          // The recorder may have transitioned to inactive between the state
          // check and stop(). Its onstop callback remains the sole finalizer.
        }
      }
      return;
    }

    stopTracks();
    resetRecordingState();
  };

  const scheduleRecordingLimit = () => {
    clearRecordingLimitTimer();
    const remaining = remainingActiveRecordingMs(
      maxScreenRecordingDurationMs,
      recordingActiveElapsedMsRef.current,
      recordingActiveStartedAtRef.current,
      performance.now(),
    );
    if (remaining <= 0) {
      stopScreenRecording("duration-limit");
      return;
    }
    recordingLimitTimerRef.current = window.setTimeout(
      () => stopScreenRecording("duration-limit"),
      remaining,
    );
  };

  const pauseScreenRecording = () => {
    const recorder = recorderRef.current;
    if (recordingPhaseRef.current !== "recording" || recorder?.state !== "recording") {
      return;
    }
    const now = performance.now();
    const elapsed = activeRecordingElapsedMs(
      recordingActiveElapsedMsRef.current,
      recordingActiveStartedAtRef.current,
      now,
    );
    try {
      recorder.pause();
    } catch (error) {
      onToast(error instanceof Error ? error.message : "未能暂停屏幕录制。");
      return;
    }
    recordingActiveElapsedMsRef.current = elapsed;
    recordingActiveStartedAtRef.current = null;
    clearRecordingLimitTimer();
    setRecordingElapsed(elapsed);
    transitionRecordingPhase("paused");
    onToast("录制已暂停；暂停期间不计入 30 分钟录制上限。");
  };

  const resumeScreenRecording = () => {
    const recorder = recorderRef.current;
    if (recordingPhaseRef.current !== "paused" || recorder?.state !== "paused") {
      return;
    }
    const remaining = remainingActiveRecordingMs(
      maxScreenRecordingDurationMs,
      recordingActiveElapsedMsRef.current,
      null,
      performance.now(),
    );
    if (remaining <= 0) {
      stopScreenRecording("duration-limit");
      return;
    }
    try {
      recorder.resume();
    } catch (error) {
      onToast(error instanceof Error ? error.message : "未能继续屏幕录制。");
      return;
    }
    recordingActiveStartedAtRef.current = performance.now();
    setRecordingElapsed(recordingActiveElapsedMsRef.current);
    transitionRecordingPhase("recording");
    scheduleRecordingLimit();
    onToast("录制已继续。");
  };

  const closeToolbox = () => {
    if (recordingPhaseRef.current === "starting") {
      cancelPendingRecordingStart();
    } else if (recordingPhaseRef.current !== "idle") {
      stopScreenRecording("drawer-closed");
    }
    onClose();
  };

  useEffect(() => {
    if (recordingPhase !== "recording") {
      return;
    }
    const updateElapsed = () => {
      setRecordingElapsed(activeRecordingElapsedMs(
        recordingActiveElapsedMsRef.current,
        recordingActiveStartedAtRef.current,
        performance.now(),
      ));
    };
    updateElapsed();
    const timer = window.setInterval(updateElapsed, 250);
    return () => window.clearInterval(timer);
  }, [recordingPhase]);

  useEffect(() => {
    if (!open || activeTab !== "time") {
      return;
    }
    const refresh = () => setCurrentTimeMilliseconds(Date.now());
    refresh();
    const timer = window.setInterval(refresh, 1_000);
    return () => window.clearInterval(timer);
  }, [activeTab, open]);

  useEffect(() => {
    if (open) {
      return;
    }
    if (recordingPhaseRef.current === "starting") {
      cancelPendingRecordingStart();
    } else if (recordingPhaseRef.current !== "idle") {
      stopScreenRecording("drawer-closed");
    }
  }, [open]);

  useEffect(() => {
    if (activeTab !== "record" && recordingPhaseRef.current !== "idle") {
      stopScreenRecording("drawer-closed");
    }
  }, [activeTab]);

  useEffect(
    () => {
      mountedRef.current = true;
      return () => {
        mountedRef.current = false;
        const cloudConnectionId = webDavConnectionIdRef.current;
        webDavConnectionIdRef.current = null;
        if (cloudConnectionId && isDesktop()) {
          void command("disconnect_webdav", {
            request: buildCloudDriveDisconnectRequest({ connectionId: cloudConnectionId }),
          }).catch(() => undefined);
        }
        recordingStartAttemptRef.current += 1;
        clearRecordingLimitTimer();
        const recorder = recorderRef.current;
        if (recorder && recorder.state !== "inactive") {
          if (!recordingStopReasonRef.current) {
            recordingStopReasonRef.current = "drawer-closed";
          }
          recordingPhaseRef.current = "stopping";
          try {
            recorder.stop();
          } catch {
            // Stopping tracks below still guarantees that no capture continues.
          }
        }
        stopTracks();
      };
    },
    [],
  );

  useEffect(() => {
    if (activeTab !== "screenshot" || !pastedImage || handledPastedImageRef.current === pastedImage.blob) {
      return;
    }
    handledPastedImageRef.current = pastedImage.blob;
    const previewUrl = URL.createObjectURL(pastedImage.blob);
    replaceScreenshotPreview(previewUrl);
    setScreenshotPreviewDescription(`已粘贴 ${pastedImage.name}；可预览或按需下载，iHub 不会把它加入剪贴板历史。`);
    setScreenshotDownloadName(pastedImage.name || "ihub-pasted-image.png");
    onPastedImageConsumed?.();
    onToast("已将粘贴图片交给截图工具；不会自动保存。");
  }, [activeTab, onPastedImageConsumed, onToast, pastedImage, replaceScreenshotPreview]);

  useEffect(() => {
    if (
      !open
      || !launchContext
    ) {
      return;
    }

    // Focus is intentionally scheduled even if this payload was already
    // applied. React Strict Mode replays effects in development; sharing the
    // one-shot data guard with the focus work would cancel the only frame and
    // leave the prefilled calculator on the document body.
    if (activeTab === "calculator" && launchContext.calculatorInput !== undefined) {
      if (handledLaunchContextRequestRef.current !== launchContext.requestId) {
        handledLaunchContextRequestRef.current = launchContext.requestId;
        setCalculatorInput(launchContext.calculatorInput);
      }
      const frame = window.requestAnimationFrame(() => {
        calculatorInputRef.current?.focus();
        calculatorInputRef.current?.select();
      });
      return () => window.cancelAnimationFrame(frame);
    }

    if (activeTab === "time" && launchContext.timeInput !== undefined) {
      if (handledLaunchContextRequestRef.current !== launchContext.requestId) {
        handledLaunchContextRequestRef.current = launchContext.requestId;
        setTimeInput(launchContext.timeInput);
      }
      const frame = window.requestAnimationFrame(() => {
        timeInputRef.current?.focus();
        timeInputRef.current?.select();
      });
      return () => window.cancelAnimationFrame(frame);
    }

    if (handledLaunchContextRequestRef.current === launchContext.requestId) {
      return;
    }

    if (activeTab === "json" && launchContext.jsonInput !== undefined) {
      handledLaunchContextRequestRef.current = launchContext.requestId;
      setJsonInput(launchContext.jsonInput);
      return;
    }

    if (activeTab === "rename" && launchContext.renameDirectory !== undefined) {
      handledLaunchContextRequestRef.current = launchContext.requestId;
      setRenameDirectory(launchContext.renameDirectory);
      setRenameDirectoryOpenId(launchContext.renameDirectoryOpenId ?? null);
      setRenamePreview(null);
    }
  }, [activeTab, launchContext, open]);

  useEffect(
    () => () => {
      revokeScreenshotObjectUrl(screenshotPreviewUrlRef.current);
      if (regionCaptureSourceRef.current?.revokeOnClose) {
        revokeScreenshotObjectUrl(regionCaptureSourceRef.current.url);
      }
    },
    [],
  );

  useEffect(() => {
    if (!open || activeTab !== "clipboard" || !isDesktop()) {
      return;
    }

    let cancelled = false;
    setIsLoadingClipboardHistory(true);
    void command<ClipboardHistorySnapshot>("get_clipboard_history", { limit: 60 })
      .then((snapshot) => {
        if (!cancelled) {
          applyClipboardHistorySnapshot(snapshot);
        }
      })
      .catch((error) => {
        if (!cancelled) {
          onToast(error instanceof Error ? error.message : "无法读取剪贴板历史。");
        }
      })
      .finally(() => {
        if (!cancelled) {
          setIsLoadingClipboardHistory(false);
        }
      });

    return () => {
      cancelled = true;
    };
  }, [activeTab, applyClipboardHistorySnapshot, onToast, open]);

  useEffect(() => {
    try {
      window.localStorage.setItem(quickNotesStorageKey, JSON.stringify(quickNotes));
    } catch {
      // Storage can be disabled by the underlying WebView. The current session remains usable.
    }
  }, [quickNotes]);

  useEffect(() => {
    try {
      window.localStorage.setItem(calculatorHistoryStorageKey, JSON.stringify(calculatorHistory));
    } catch {
      // Storage can be disabled by the underlying WebView. The current calculation remains usable.
    }
  }, [calculatorHistory]);

  const copyText = async (value: string, label: string) => {
    try {
      if (isDesktop()) {
        await command("write_clipboard_text", { text: value });
      } else if (navigator.clipboard) {
        await navigator.clipboard.writeText(value);
      } else {
        throw new Error("当前环境不允许访问剪贴板。");
      }
      onToast(`${label} 已复制到剪贴板。`);
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法复制到剪贴板。");
    }
  };

  const commitCalculator = () => {
    const expression = calculatorInput.trim();
    if (!expression) {
      onToast("先输入一个计算表达式。");
      return;
    }
    if (!calculatorEvaluation.valid || !calculatorEvaluation.formatted) {
      onToast(calculatorEvaluation.error ?? "这个表达式无法计算。");
      return;
    }
    const result = calculatorEvaluation.formatted;
    setCalculatorHistory((current) => [
      {
        id: calculatorHistoryId(),
        expression,
        result,
        createdAt: Date.now(),
      },
      ...current.filter((entry) => entry.expression !== expression || entry.result !== result),
    ].slice(0, 24));
    onToast(`结果 ${result} 已加入本机计算历史。`);
  };

  const appendCalculatorKey = (key: (typeof calculatorKeys)[number]) => {
    if (key === "=") {
      commitCalculator();
      return;
    }
    if (key === "⌫") {
      setCalculatorInput((current) => current.slice(0, -1));
      return;
    }
    setCalculatorInput((current) => `${current}${key}`);
  };

  const generateQrCode = async () => {
    if (!qrInput.trim()) {
      const message = "请输入要生成二维码的文本或 URL。";
      setQrError(message);
      onToast(message);
      return;
    }

    setIsGeneratingQr(true);
    setQrError(null);
    try {
      // QR encoding is only needed after the user opens this optional tool;
      // loading it here keeps the Spotlight launch bundle lean.
      const { toDataURL } = await import("qrcode");
      const dataUrl = await toDataURL(qrInput, {
        color: {
          dark: "#101419ff",
          light: "#ffffffff",
        },
        errorCorrectionLevel: "M",
        margin: 2,
        type: "image/png",
        width: 720,
      });
      setQrPreviewUrl(dataUrl);
      onToast("二维码已生成，可直接下载 PNG。");
    } catch (error) {
      const message = error instanceof Error ? error.message : "二维码生成失败。请缩短内容后重试。";
      setQrPreviewUrl(null);
      setQrError(message);
      onToast(`二维码生成失败：${message}`);
    } finally {
      setIsGeneratingQr(false);
    }
  };

  const downloadQrCode = () => {
    if (!qrPreviewUrl) {
      onToast("请先生成二维码。");
      return;
    }

    const anchor = document.createElement("a");
    anchor.href = qrPreviewUrl;
    anchor.download = `ihub-qrcode-${new Date().toISOString().replace(/[:.]/g, "-")}.png`;
    anchor.click();
    onToast("二维码 PNG 已交给系统下载。");
  };

  const decodeQrImage = async (file: File) => {
    setIsDecodingQr(true);
    setQrDecodeFileName(file.name);
    setQrDecodeValue(null);
    setQrDecodeError(null);
    try {
      const value = await decodeQrImageFile(file);
      if (!value) {
        throw new Error("没有在这张图片中识别到二维码。请使用更清晰、完整的二维码图片重试。");
      }
      setQrDecodeValue(value);
      onToast("已在本地识别二维码内容；你可以复制或填入生成器。");
    } catch (error) {
      const message = error instanceof Error ? error.message : "二维码图片识别失败。";
      setQrDecodeError(message);
      onToast(`二维码识别失败：${message}`);
    } finally {
      setIsDecodingQr(false);
    }
  };

  const acceptWebDavDirectory = (
    response: WebDavDirectoryResponse,
    connectionId: string,
    profile: CloudProfileView | null = null,
  ) => {
    const confirmedRoot = parseWebDavEndpoint(response.endpoint);
    const confirmedDirectory = new URL(response.directory);
    confirmedDirectory.search = "";
    confirmedDirectory.hash = "";
    if (!isWebDavUrlWithinRoot(confirmedRoot, confirmedDirectory)) {
      throw new Error("WebDAV 服务返回了根目录之外的地址，已拒绝显示。");
    }
    const entries = parseWebDavDirectoryXml(response.xml, confirmedRoot, confirmedDirectory);
    webDavConnectionIdRef.current = connectionId;
    setWebDavConnectionId(connectionId);
    setWebDavConnectedProfileId(profile?.id ?? null);
    setWebDavConnectedRoot(confirmedRoot.href);
    setWebDavDirectory(confirmedDirectory.href);
    setWebDavEntries(entries);
    return entries;
  };

  const refreshCloudProfiles = async () => {
    if (!isDesktop()) {
      return;
    }
    setIsLoadingCloudProfiles(true);
    try {
      const profiles = await command<CloudProfileView[]>("list_cloud_profiles");
      if (mountedRef.current) {
        setCloudProfiles(profiles);
      }
    } catch (error) {
      if (mountedRef.current) {
        const message = error instanceof Error ? error.message.trim() : "无法读取已保存的云盘连接。";
        setWebDavError(message);
      }
    } finally {
      if (mountedRef.current) {
        setIsLoadingCloudProfiles(false);
      }
    }
  };

  const connectWebDav = async () => {
    if (!isDesktop()) {
      const message = "云盘连接仅在 iHub 桌面版中提供。";
      setWebDavError(message);
      onToast(message);
      return;
    }

    let root: URL;
    try {
      root = parseWebDavEndpoint(webDavEndpoint);
    } catch (error) {
      const message = error instanceof Error ? error.message.trim() : "WebDAV 地址无效。";
      setWebDavError(message);
      onToast(message);
      return;
    }

    const requestId = webDavRequestIdRef.current + 1;
    webDavRequestIdRef.current = requestId;
    setIsLoadingWebDav(true);
    setWebDavError(null);
    try {
      const response = await command<WebDavConnectResult>("connect_webdav", {
        request: buildWebDavConnectRequest({
          endpoint: root.href,
          username: webDavUsername,
          password: webDavPassword,
          remember: webDavRemember,
          ...(webDavRemember
            ? { label: webDavProfileLabel.trim() || root.hostname }
            : {}),
        }),
      });
      if (!mountedRef.current || webDavRequestIdRef.current !== requestId) {
        void command("disconnect_webdav", {
          request: buildCloudDriveDisconnectRequest({ connectionId: response.connectionId }),
        }).catch(() => undefined);
        return;
      }
      const entries = acceptWebDavDirectory(response, response.connectionId, response.profile);
      setWebDavEndpoint(response.endpoint);
      setWebDavProfileLabel("");
      setWebDavRemember(false);
      if (response.profile) {
        void refreshCloudProfiles();
      }
      onToast(`已读取 WebDAV 目录，共 ${entries.length} 项。`);
    } catch (error) {
      if (!mountedRef.current || webDavRequestIdRef.current !== requestId) {
        return;
      }
      const message = error instanceof Error ? error.message.trim() : "无法读取 WebDAV 目录。";
      setWebDavError(message);
      onToast(`云盘连接失败：${message}`);
    } finally {
      // JavaScript strings cannot be reliably zeroized, so minimize their
      // lifetime: one explicit connect attempt is the only time the renderer
      // retains and sends this password.
      setWebDavPassword("");
      if (mountedRef.current && webDavRequestIdRef.current === requestId) {
        setIsLoadingWebDav(false);
      }
    }
  };

  const connectSavedCloudProfile = async (profile: CloudProfileView) => {
    if (!isDesktop() || webDavConnectionIdRef.current) {
      return;
    }
    const requestId = webDavRequestIdRef.current + 1;
    webDavRequestIdRef.current = requestId;
    setConnectingCloudProfileId(profile.id);
    setIsLoadingWebDav(true);
    setWebDavError(null);
    try {
      const response = await command<WebDavConnectResult>("connect_cloud_profile", {
        request: buildWebDavSavedConnectRequest({ profileId: profile.id }),
      });
      if (!mountedRef.current || webDavRequestIdRef.current !== requestId) {
        void command("disconnect_webdav", {
          request: buildCloudDriveDisconnectRequest({ connectionId: response.connectionId }),
        }).catch(() => undefined);
        return;
      }
      const entries = acceptWebDavDirectory(response, response.connectionId, response.profile ?? profile);
      setWebDavEndpoint(response.endpoint);
      setWebDavUsername(profile.username);
      setWebDavPassword("");
      onToast(`已连接 ${profile.label}，共 ${entries.length} 项。`);
    } catch (error) {
      if (!mountedRef.current || webDavRequestIdRef.current !== requestId) {
        return;
      }
      const message = error instanceof Error ? error.message.trim() : "无法连接已保存的云盘。";
      setWebDavError(message);
      onToast(`云盘连接失败：${message}`);
    } finally {
      if (mountedRef.current && webDavRequestIdRef.current === requestId) {
        setIsLoadingWebDav(false);
        setConnectingCloudProfileId(null);
      }
    }
  };

  const loadWebDavDirectory = async (requestedDirectory?: string) => {
    const connectionId = webDavConnectionIdRef.current;
    if (!isDesktop() || !connectionId || !webDavConnectedRoot) {
      onToast("请先连接 WebDAV。");
      return;
    }

    let directory: URL;
    try {
      const root = parseWebDavEndpoint(webDavConnectedRoot);
      directory = requestedDirectory ? new URL(requestedDirectory) : root;
      directory.search = "";
      directory.hash = "";
      if (!isWebDavUrlWithinRoot(root, directory)) {
        throw new Error("WebDAV 目录必须位于当前连接根目录内。请重新连接。");
      }
    } catch (error) {
      const message = error instanceof Error ? error.message.trim() : "WebDAV 地址无效。";
      setWebDavError(message);
      onToast(message);
      return;
    }

    const requestId = webDavRequestIdRef.current + 1;
    webDavRequestIdRef.current = requestId;
    setIsLoadingWebDav(true);
    setWebDavError(null);
    try {
      const response = await command<WebDavDirectoryResponse>("list_webdav_directory", {
        request: buildWebDavListRequest({
          connectionId,
          directory: directory.href,
        }),
      });
      if (!mountedRef.current || webDavRequestIdRef.current !== requestId) {
        return;
      }
      const entries = acceptWebDavDirectory(
        response,
        connectionId,
        cloudProfiles.find((profile) => profile.id === webDavConnectedProfileId) ?? null,
      );
      onToast(`已读取 WebDAV 目录，共 ${entries.length} 项。`);
    } catch (error) {
      if (!mountedRef.current || webDavRequestIdRef.current !== requestId) {
        return;
      }
      const message = error instanceof Error ? error.message.trim() : "无法读取 WebDAV 目录。";
      setWebDavError(message);
      onToast(`云盘读取失败：${message}`);
    } finally {
      if (mountedRef.current && webDavRequestIdRef.current === requestId) {
        setIsLoadingWebDav(false);
      }
    }
  };

  const clearWebDavSession = () => {
    webDavRequestIdRef.current += 1;
    webDavConnectionIdRef.current = null;
    setWebDavConnectionId(null);
    setWebDavConnectedProfileId(null);
    setWebDavPassword("");
    setWebDavConnectedRoot(null);
    setWebDavDirectory(null);
    setWebDavEntries([]);
    setWebDavError(null);
    setIsLoadingWebDav(false);
    setWebDavDownloadingHref(null);
    setIsUploadingWebDav(false);
    setConnectingCloudProfileId(null);
  };

  const disconnectWebDav = async () => {
    const connectionId = webDavConnectionIdRef.current;
    clearWebDavSession();
    if (!connectionId || !isDesktop()) {
      return;
    }
    try {
      await command("disconnect_webdav", {
        request: buildCloudDriveDisconnectRequest({ connectionId }),
      });
      onToast("已断开云盘；已保存的连接仍保留在系统凭据库。");
    } catch (error) {
      onToast(error instanceof Error ? error.message : "本机会话将在超时后自动清理。");
    }
  };

  const forgetCloudProfile = async (profile: CloudProfileView) => {
    if (!isDesktop() || forgettingCloudProfileId) {
      return;
    }
    if (!window.confirm(`忘记“${profile.label}”？这只会删除 iHub 保存的系统凭据，不会删除云盘中的文件。`)) {
      return;
    }
    setForgettingCloudProfileId(profile.id);
    setWebDavError(null);
    try {
      await command("forget_cloud_profile", {
        request: buildCloudProfileForgetRequest({ profileId: profile.id }),
      });
      if (webDavConnectedProfileId === profile.id) {
        clearWebDavSession();
      }
      setCloudProfiles((current) => current.filter((candidate) => candidate.id !== profile.id));
      onToast(`已从系统凭据库忘记 ${profile.label}。`);
    } catch (error) {
      const message = error instanceof Error ? error.message.trim() : "无法删除已保存的云盘连接。";
      setWebDavError(message);
      onToast(`无法忘记云盘连接：${message}`);
    } finally {
      if (mountedRef.current) {
        setForgettingCloudProfileId(null);
      }
    }
  };

  const openWebDavParent = () => {
    if (!webDavConnectedRoot || !webDavDirectory) {
      return;
    }
    try {
      const root = parseWebDavEndpoint(webDavConnectedRoot);
      const current = new URL(webDavDirectory);
      const parent = new URL("../", current);
      if (!isWebDavUrlWithinRoot(root, parent) || parent.href === current.href) {
        onToast("当前已在 WebDAV 连接根目录。");
        return;
      }
      void loadWebDavDirectory(parent.href);
    } catch {
      onToast("无法返回上一级目录；请重新连接 WebDAV。");
    }
  };

  const downloadWebDavFile = async (entry: WebDavEntry) => {
    const connectionId = webDavConnectionIdRef.current;
    if (!isDesktop() || !connectionId) {
      onToast("云盘下载仅在 iHub 桌面版中提供。");
      return;
    }
    if (entry.isCollection) {
      return;
    }
    setWebDavDownloadingHref(entry.href);
    setWebDavError(null);
    try {
      const result = await command<WebDavDownloadResult>("download_webdav_file", {
        request: buildWebDavDownloadRequest({
          connectionId,
          remoteUrl: entry.href,
          suggestedFilename: entry.name,
        }),
      });
      if (!mountedRef.current) {
        return;
      }
      if (result.cancelled) {
        onToast("已取消云盘下载。");
        return;
      }
      onToast(`已保存 ${result.filename ?? entry.name}（${formatWebDavBytes(result.bytesWritten)}）。`);
    } catch (error) {
      if (!mountedRef.current) {
        return;
      }
      const message = error instanceof Error ? error.message.trim() : "云盘下载失败。";
      setWebDavError(message);
      onToast(`云盘下载失败：${message}`);
    } finally {
      if (mountedRef.current) {
        setWebDavDownloadingHref((current) => current === entry.href ? null : current);
      }
    }
  };

  const uploadWebDavFile = async () => {
    const connectionId = webDavConnectionIdRef.current;
    if (!isDesktop() || !connectionId || !webDavDirectory) {
      onToast("请先连接 WebDAV 后再上传文件。");
      return;
    }
    setIsUploadingWebDav(true);
    setWebDavError(null);
    try {
      const result = await command<WebDavUploadResult>("upload_webdav_file", {
        request: buildWebDavUploadRequest({
          connectionId,
          directory: webDavDirectory,
        }),
      });
      if (!mountedRef.current) {
        return;
      }
      if (result.cancelled) {
        onToast("已取消云盘上传。");
        return;
      }
      onToast(`已上传 ${result.filename ?? "文件"}（${formatWebDavBytes(result.bytesWritten)}）。`);
      void loadWebDavDirectory(webDavDirectory);
    } catch (error) {
      if (!mountedRef.current) {
        return;
      }
      const message = error instanceof Error ? error.message.trim() : "云盘上传失败。";
      setWebDavError(message);
      onToast(`云盘上传失败：${message}`);
    } finally {
      if (mountedRef.current) {
        setIsUploadingWebDav(false);
      }
    }
  };

  useEffect(() => {
    if (open && activeTab === "cloud" && isDesktop()) {
      void refreshCloudProfiles();
      return;
    }

    const connectionId = webDavConnectionIdRef.current;
    if (!connectionId) {
      return;
    }
    webDavRequestIdRef.current += 1;
    webDavConnectionIdRef.current = null;
    setWebDavConnectionId(null);
    setWebDavConnectedProfileId(null);
    setWebDavPassword("");
    setWebDavConnectedRoot(null);
    setWebDavDirectory(null);
    setWebDavEntries([]);
    setWebDavError(null);
    setIsLoadingWebDav(false);
    setWebDavDownloadingHref(null);
    setIsUploadingWebDav(false);
    setConnectingCloudProfileId(null);
    void command("disconnect_webdav", {
      request: buildCloudDriveDisconnectRequest({ connectionId }),
    }).catch(() => undefined);
  }, [activeTab, open]);

  const pickScreenColor = async () => {
    const EyeDropper = eyeDropperConstructor();
    if (!EyeDropper) {
      onToast("当前 WebView 不支持系统屏幕吸管；可继续点击色块选择颜色。");
      return;
    }

    try {
      const picked = await new EyeDropper().open();
      setColor(picked.sRGBHex);
      onToast(`已拾取 ${picked.sRGBHex.toUpperCase()}。`);
    } catch (error) {
      if (error instanceof DOMException && error.name === "AbortError") {
        return;
      }
      onToast(error instanceof Error ? error.message : "无法从屏幕拾取颜色。");
    }
  };

  const saveQuickNote = () => {
    const text = quickNoteDraft.trim();
    if (!text) {
      onToast("先输入一条便签内容。");
      return;
    }

    const now = Date.now();
    setQuickNotes((current) => [
      { id: createQuickNoteId(), text, createdAt: now, updatedAt: now },
      ...current,
    ].slice(0, 100));
    setQuickNoteDraft("");
    onToast("便签已保存在本机。");
  };

  const deleteQuickNote = (note: QuickNote) => {
    if (!window.confirm(`删除便签“${noteTitle(note.text)}”？此操作无法撤销。`)) {
      return;
    }
    setQuickNotes((current) => current.filter((item) => item.id !== note.id));
    onToast("便签已删除。");
  };

  const captureScreenshot = async () => {
    if (!navigator.mediaDevices?.getDisplayMedia) {
      onToast("当前 WebView 不支持系统屏幕选择器，无法截图。");
      return;
    }

    let stream: MediaStream | null = null;
    setIsCapturingScreenshot(true);
    try {
      stream = await getDisplayMediaWithFocusLease({ audio: false, video: true });
      const track = stream.getVideoTracks()[0];
      if (!track) {
        throw new Error("系统没有返回可用的视频轨道。");
      }

      const video = document.createElement("video");
      video.muted = true;
      video.playsInline = true;
      video.srcObject = stream;
      await new Promise<void>((resolve, reject) => {
        video.onloadedmetadata = () => resolve();
        video.onerror = () => reject(new Error("无法读取所选屏幕的画面。"));
      });
      await video.play();
      await new Promise<void>((resolve) => {
        window.requestAnimationFrame(() => window.requestAnimationFrame(() => resolve()));
      });

      const settings = track.getSettings();
      const width = video.videoWidth || settings.width || 0;
      const height = video.videoHeight || settings.height || 0;
      if (!width || !height) {
        throw new Error("无法确定截图尺寸。");
      }
      validateRegionCaptureSize({ width, height });

      const canvas = document.createElement("canvas");
      canvas.width = width;
      canvas.height = height;
      const context = canvas.getContext("2d");
      if (!context) {
        throw new Error("当前 WebView 无法创建截图画布。");
      }
      context.drawImage(video, 0, 0, width, height);
      video.pause();
      video.srcObject = null;

      const blob = await new Promise<Blob>((resolve, reject) => {
        canvas.toBlob((image) => {
          if (image) {
            resolve(image);
          } else {
            reject(new Error("系统没有生成 PNG 图片。"));
          }
        }, "image/png");
      });

      const fileName = `ihub-capture-${new Date().toISOString().replace(/[:.]/g, "-")}.png`;
      replaceRegionCaptureSource({
        height,
        name: fileName,
        revokeOnClose: true,
        url: URL.createObjectURL(blob),
        width,
      });
      onToast("画面已就绪；请拖拽选择要导出的矩形区域。");
    } catch (error) {
      onToast(error instanceof Error ? error.message : "未能完成截图。");
    } finally {
      stream?.getTracks().forEach((track) => track.stop());
      setIsCapturingScreenshot(false);
    }
  };

  const capturePrimaryMonitorNatively = async () => {
    if (!isDesktop()) {
      onToast("原生显示器帧仅在 iHub 桌面应用中提供。");
      return;
    }
    if (isCapturingScreenshot) {
      return;
    }

    setIsCapturingScreenshot(true);
    try {
      const screenshot = await command<NativeScreenshot>("capture_native_screenshot");
      if (screenshot.mimeType !== "image/png" || !screenshot.dataUrl.startsWith("data:image/png;base64,")) {
        throw new Error("原生截图返回了无效的 PNG 数据。");
      }
      const fileName = screenshot.name.trim() || `ihub-monitor-${screenshot.displayIndex + 1}.png`;
      validateRegionCaptureSize(screenshot);
      replaceRegionCaptureSource({
        height: screenshot.height,
        name: fileName,
        url: screenshot.dataUrl,
        width: screenshot.width,
      });
      onToast("已读取显示器的一帧；请拖拽选择要导出的矩形区域。");
    } catch (error) {
      onToast(error instanceof Error ? error.message : "未能完成原生截图。");
    } finally {
      setIsCapturingScreenshot(false);
    }
  };

  const loadRegionCaptureDemo = () => {
    replaceRegionCaptureSource(createRegionCaptureDemoSource());
    onToast("已载入本地模拟画面；可在浏览器中验证拖拽、取消与导出。");
  };

  const exportRegionCapture = async (capture: CroppedCapture) => {
    const previewUrl = URL.createObjectURL(capture.blob);
    replaceScreenshotPreview(previewUrl);
    setScreenshotDownloadName(capture.name);
    setScreenshotPreviewDescription(
      `已裁剪 ${capture.width} × ${capture.height} PNG；只保留在当前预览，点击下载才会保存文件。`,
    );
    replaceRegionCaptureSource(null);
    onToast(`已导出 ${capture.width} × ${capture.height} 的矩形选区。`);
  };

  const downloadScreenshotPreview = () => {
    if (!screenshotPreviewUrl) {
      onToast("请先截图或粘贴一张图片。");
      return;
    }
    const anchor = document.createElement("a");
    anchor.href = screenshotPreviewUrl;
    anchor.download = screenshotDownloadName;
    anchor.click();
    onToast("图片已交给系统下载。");
  };

  const startScreenRecording = async () => {
    if (!navigator.mediaDevices?.getDisplayMedia || typeof MediaRecorder === "undefined") {
      onToast("当前 WebView 不支持屏幕录制；官方 Screen Recorder 同样依赖系统的屏幕共享与 MediaRecorder 能力。");
      return;
    }

    // A double-click can otherwise open two system pickers before React has a
    // chance to render the disabled state. Keep this phase gate synchronous.
    if (recordingPhaseRef.current !== "idle" || recorderRef.current) {
      return;
    }

    const attemptId = ++recordingStartAttemptRef.current;
    let stream: MediaStream | null = null;
    const attemptIsCurrent = () => (
      mountedRef.current
      && openRef.current
      && recordingStartAttemptRef.current === attemptId
      && recordingPhaseRef.current === "starting"
    );
    transitionRecordingPhase("starting");
    try {
      stream = await getDisplayMediaWithFocusLease({
        audio: includeSystemAudio,
        video: true,
      });
      // The system picker is asynchronous. Closing the toolbox while it is
      // visible invalidates this attempt, so a late stream is stopped before a
      // MediaRecorder can ever be created.
      if (!attemptIsCurrent()) {
        stream.getTracks().forEach((track) => track.stop());
        return;
      }
      const supportedMimeType = [
        "video/webm;codecs=vp9,opus",
        "video/webm;codecs=vp8,opus",
        "video/webm",
      ].find((mimeType) => MediaRecorder.isTypeSupported(mimeType));
      const recorder = supportedMimeType
        ? new MediaRecorder(stream, { mimeType: supportedMimeType })
        : new MediaRecorder(stream);

      recordingStreamRef.current = stream;
      recordingChunksRef.current = [];
      recordingBytesRef.current = 0;
      recordingStopReasonRef.current = null;
      setRecordingBytes(0);
      recorderRef.current = recorder;
      recorder.ondataavailable = (event) => {
        if (event.data.size > 0) {
          recordingChunksRef.current.push(event.data);
          recordingBytesRef.current += event.data.size;
          if (mountedRef.current) {
            setRecordingBytes(recordingBytesRef.current);
          }

          // `MediaRecorder` emits one-second chunks. The final file can exceed
          // the guard by at most the active chunk, but we retain it so the WebM
          // remains complete and playable.
          if (recordingBytesRef.current >= maxScreenRecordingBytes) {
            stopScreenRecording("size-limit");
          }
        }
      };
      recorder.onstop = () => {
        const chunks = recordingChunksRef.current;
        recordingChunksRef.current = [];
        const stopReason = recordingStopReasonRef.current;
        recordingStopReasonRef.current = null;
        const mimeType = recorder.mimeType || "video/webm";
        if (chunks.length) {
          const blob = new Blob(chunks, { type: mimeType });
          saveBlob(blob, `ihub-screen-${new Date().toISOString().replace(/[:.]/g, "-")}.webm`);
          const completionMessage = stopReason === "duration-limit"
            ? "已达到 30 分钟录制上限，已自动停止并下载 WebM。"
            : stopReason === "size-limit"
              ? "已达到 512 MB 录制上限，已自动停止并下载 WebM。"
              : stopReason === "source-ended"
                ? "屏幕共享已结束，已下载已录制的 WebM。"
                : stopReason === "error"
                  ? "录制意外结束，已下载可恢复的 WebM 内容。"
                  : "录屏已结束，WebM 文件已交给系统下载。";
          if (mountedRef.current) {
            onToast(completionMessage);
          }
        } else if (mountedRef.current) {
          onToast("录制已结束，但没有生成可保存的视频片段。");
        }
        stopTracks();
        if (recorderRef.current === recorder) {
          recorderRef.current = null;
        }
        resetRecordingState();
      };
      recorder.onerror = () => stopScreenRecording("error");
      stream.getVideoTracks()[0]?.addEventListener(
        "ended",
        () => stopScreenRecording("source-ended"),
        { once: true },
      );
      recorder.start(1_000);
      recordingActiveElapsedMsRef.current = 0;
      recordingActiveStartedAtRef.current = performance.now();
      setRecordingElapsed(0);
      transitionRecordingPhase("recording");
      scheduleRecordingLimit();
      onToast("正在录制屏幕；最多 30 分钟或 512 MB，关闭工具箱也会结束并保存录制。");
    } catch (error) {
      if (!attemptIsCurrent()) {
        stream?.getTracks().forEach((track) => track.stop());
        return;
      }
      clearRecordingLimitTimer();
      const streamIsTracked = recordingStreamRef.current === stream;
      stopTracks();
      if (!streamIsTracked) {
        stream?.getTracks().forEach((track) => track.stop());
      }
      recorderRef.current = null;
      recordingChunksRef.current = [];
      recordingBytesRef.current = 0;
      recordingStopReasonRef.current = null;
      recordingActiveElapsedMsRef.current = 0;
      recordingActiveStartedAtRef.current = null;
      setRecordingBytes(0);
      setRecordingElapsed(0);
      transitionRecordingPhase("idle");
      onToast(error instanceof Error ? error.message : "未能开始屏幕录制。");
    }
  };

  const refreshClipboardHistory = async () => {
    if (!isDesktop()) {
      onToast("剪贴板历史仅在 iHub 桌面端中运行。");
      return;
    }
    setIsLoadingClipboardHistory(true);
    try {
      const snapshot = await command<ClipboardHistorySnapshot>("get_clipboard_history", { limit: 60 });
      applyClipboardHistorySnapshot(snapshot);
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法读取剪贴板历史。");
    } finally {
      setIsLoadingClipboardHistory(false);
    }
  };

  const setClipboardHistoryEnabled = async (enabled: boolean) => {
    if (!isDesktop()) {
      onToast("剪贴板历史仅在 iHub 桌面端中运行。");
      return;
    }
    setClipboardActionId("history-enabled");
    try {
      const snapshot = await command<ClipboardHistorySnapshot>("set_clipboard_history_enabled", { enabled });
      applyClipboardHistorySnapshot(snapshot);
      onToast(enabled
        ? "剪贴板历史已开启；从现在起仅在本机记录文本。"
        : "剪贴板历史已暂停；图片和文件采集也已关闭，已有记录会保留，直到你删除它们。",
      );
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法更新剪贴板历史设置。");
    } finally {
      setClipboardActionId(null);
    }
  };

  const setClipboardHistoryCaptureOptions = async (
    imageHistoryEnabled: boolean,
    fileHistoryEnabled: boolean,
  ) => {
    if (!isDesktop() || !clipboardHistory?.enabled) {
      return;
    }
    setClipboardActionId("history-options");
    try {
      const snapshot = await command<ClipboardHistorySnapshot>("set_clipboard_history_capture_options", {
        imageHistoryEnabled,
        fileHistoryEnabled,
      });
      applyClipboardHistorySnapshot(snapshot);
      onToast(
        imageHistoryEnabled || fileHistoryEnabled
          ? "已更新额外格式采集设置；iHub 不会上传内容。"
          : "图片和文件引用采集已关闭；已有记录仍由你决定何时删除。",
      );
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法更新剪贴板历史格式设置。");
    } finally {
      setClipboardActionId(null);
    }
  };

  const restoreClipboardHistoryItem = async (item: ClipboardHistoryItem) => {
    if (!isDesktop()) {
      return;
    }
    setClipboardActionId(`${item.id}:restore`);
    try {
      const result = await command<ClipboardHistoryRestoreResult>("restore_clipboard_history_item", { id: item.id });
      await refreshClipboardHistory();
      if (result.kind === "files") {
        onToast(`已将 ${result.restoredCount} 个已验证文件引用放回系统剪贴板。`);
      } else if (result.kind === "image") {
        onToast("已将图片还原到系统剪贴板。");
      } else {
        onToast("已复制到系统剪贴板。");
      }
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法还原这条剪贴板记录。");
    } finally {
      setClipboardActionId(null);
    }
  };

  const previewClipboardHistoryImage = async (item: ClipboardHistoryItem) => {
    if (!isDesktop() || item.kind !== "image") {
      return;
    }
    setClipboardActionId(`${item.id}:preview`);
    try {
      const image = await command<ClipboardImage>("get_clipboard_history_image_preview", { id: item.id });
      setClipboardImagePreview({ id: item.id, image });
      onToast("已按你的请求载入本地图片预览。预览不会自动保存到其他位置。");
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法读取这张剪贴板历史图片。");
    } finally {
      setClipboardActionId(null);
    }
  };

  const openClipboardHistoryFileEntry = async (item: ClipboardHistoryItem, fileIndex: number) => {
    if (!isDesktop() || item.kind !== "files") {
      return;
    }
    setClipboardActionId(`${item.id}:open:${fileIndex}`);
    try {
      await command<void>("open_clipboard_history_file_entry", { id: item.id, fileIndex });
      onToast("已在系统中打开经过重新验证的文件项目。");
    } catch (error) {
      onToast(error instanceof Error ? error.message : "该文件项目已变更或不可用，未执行打开操作。");
    } finally {
      setClipboardActionId(null);
    }
  };

  const setClipboardHistoryPinned = async (id: string, pinned: boolean) => {
    if (!isDesktop()) {
      return;
    }
    setClipboardActionId(`${id}:pin`);
    try {
      const snapshot = await command<ClipboardHistorySnapshot>("set_clipboard_history_item_pinned", { id, pinned });
      applyClipboardHistorySnapshot(snapshot);
      onToast(pinned ? "已固定这条剪贴板内容。" : "已取消固定这条剪贴板内容。");
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法更新固定状态。");
    } finally {
      setClipboardActionId(null);
    }
  };

  const deleteClipboardHistoryItem = async (id: string) => {
    if (!isDesktop()) {
      return;
    }
    setClipboardActionId(`${id}:delete`);
    try {
      const snapshot = await command<ClipboardHistorySnapshot>("delete_clipboard_history_item", { id });
      applyClipboardHistorySnapshot(snapshot);
      onToast("已删除这条剪贴板记录。");
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法删除这条历史记录。");
    } finally {
      setClipboardActionId(null);
    }
  };

  const clearUnpinnedClipboardHistory = async () => {
    if (!isDesktop() || !clipboardHistory?.items.some((item) => !item.pinned)) {
      return;
    }
    if (!window.confirm("删除所有未固定的剪贴板记录？文本、已保存图片和文件引用都会删除；已固定的内容会保留。")) {
      return;
    }
    setClipboardActionId("clear-unpinned");
    try {
      const snapshot = await command<ClipboardHistorySnapshot>("clear_unpinned_clipboard_history");
      applyClipboardHistorySnapshot(snapshot);
      onToast("已清除未固定的剪贴板记录。");
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法清除剪贴板历史。");
    } finally {
      setClipboardActionId(null);
    }
  };

  const previewRename = async () => {
    if (!renameDirectoryOpenId) {
      onToast("请通过系统选择器重新选择需要批量重命名的文件夹。");
      return;
    }
    if (!renameFind) {
      onToast("请输入要查找的文本或正则表达式。");
      return;
    }
    if (!isDesktop()) {
      onToast("浏览器预览不会读取或修改本地文件。");
      return;
    }

    const usesSequence = renameReplace.includes("{n}");
    let sequenceStart: number | undefined;
    let sequencePadding: number | undefined;
    if (usesSequence) {
      const parsedStart = parseBoundedWholeNumber(renameSequenceStart, 4_294_967_295);
      const parsedPadding = parseBoundedWholeNumber(renameSequencePadding, 12);
      if (parsedStart === null || parsedStart < 1) {
        onToast("序号起始值必须是从 1 开始的整数。");
        return;
      }
      if (parsedPadding === null) {
        onToast("序号补零位数只能是 0 到 12 的整数。");
        return;
      }
      sequenceStart = parsedStart;
      sequencePadding = parsedPadding;
    }

    setIsPreviewingRename(true);
    try {
      const preview = await command<BatchRenamePreview>("preview_batch_rename", {
        directoryOpenId: renameDirectoryOpenId,
        find: renameFind,
        replace: renameReplace,
        useRegex: renameUseRegex,
        sequenceStart,
        sequencePadding,
      });
      setRenamePreview(preview);
      if (!preview.items.length && !preview.errors.length) {
        onToast("没有找到需要改名的直接子文件。");
      }
    } catch (error) {
      setRenamePreview(null);
      onToast(error instanceof Error ? error.message : "无法生成重命名预览。");
    } finally {
      setIsPreviewingRename(false);
    }
  };

  const applyRename = async () => {
    if (
      !renameDirectoryOpenId
      || !renamePreview?.canApply
      || !renamePreview.items.length
      || !isDesktop()
    ) {
      return;
    }
    const approved = window.confirm(
      `将立即重命名 ${renamePreview.items.length} 个文件。已在执行前再次检查冲突，是否继续？`,
    );
    if (!approved) {
      return;
    }

    setIsApplyingRename(true);
    try {
      const result = await command<BatchRenameResult>("apply_batch_rename", {
        directoryOpenId: renameDirectoryOpenId,
        items: renamePreview.items,
      });
      setRenamePreview(null);
      onToast(`已重命名 ${result.renamed} 个文件。`);
    } catch (error) {
      onToast(error instanceof Error ? error.message : "批量重命名未完成。");
    } finally {
      setIsApplyingRename(false);
    }
  };

  const createPluginProject = async () => {
    const normalizedId = projectId.trim();
    if (!projectParentDirectoryOpenId) {
      onToast("请通过系统选择器重新选择插件项目父目录。");
      return;
    }
    if (!isPluginId(normalizedId)) {
      onToast("插件 ID 需使用小写 kebab-case，例如 ihub-plugin-my-feature。");
      return;
    }
    if (!isDesktop()) {
      onToast("浏览器预览不会在磁盘上创建插件项目。");
      return;
    }

    setIsCreatingProject(true);
    try {
      const result = await command<PluginProjectResult>("create_plugin_project", {
        parentDirectoryOpenId: projectParentDirectoryOpenId,
        pluginId: normalizedId,
      });
      setProjectResult(result);
      // The host just created this canonical directory. Prefill it for the
      // separately user-confirmed link action, but never build/install/run
      // anything on the developer's behalf.
      setLocalPluginDirectory(result.projectPath);
      setLocalPluginDirectoryOpenId(result.openId);
      onToast("插件项目模板已创建；下方已填入链接目录。请先审阅并执行 pnpm build。");
    } catch (error) {
      setProjectResult(null);
      onToast(error instanceof Error ? error.message : "无法创建插件项目。");
    } finally {
      setIsCreatingProject(false);
    }
  };

  const openCreatedProject = async () => {
    const openId = projectResult?.openId;
    if (!openId) {
      return;
    }
    if (!isDesktop()) {
      onToast("浏览器预览不会打开本机项目文件夹。");
      return;
    }

    setIsOpeningProject(true);
    try {
      // The native template command returns a short-lived opaque open ID bound
      // to the exact directory it created. Typed paths never reach the opener.
      await command<void>("open_granted_path", { openId });
      onToast("已在系统文件管理器中打开项目目录。");
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法打开项目目录。");
    } finally {
      setIsOpeningProject(false);
    }
  };

  const refreshPlugins = async () => {
    const nextPlugins = await command<PluginInfo[]>("list_plugins");
    onPluginsChanged(nextPlugins);
  };

  const linkLocalPlugin = async () => {
    if (!localPluginDirectoryOpenId) {
      onToast("请通过系统选择器重新选择本地插件项目目录。");
      return;
    }
    if (!isDesktop()) {
      onToast("浏览器预览不会链接本地插件项目。");
      return;
    }

    setIsLinkingLocalPlugin(true);
    try {
      const plugin = await command<PluginInfo>("link_plugin_from_local", {
        directoryOpenId: localPluginDirectoryOpenId,
      });
      await refreshPlugins();
      setLocalPluginDirectory("");
      setLocalPluginDirectoryOpenId(null);
      onToast(`已链接 ${plugin.name}。构建后关闭再重新打开插件界面即可读取最新文件。`);
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法链接本地插件项目。");
    } finally {
      setIsLinkingLocalPlugin(false);
    }
  };

  const unlinkLocalPlugin = async (plugin: PluginInfo) => {
    if (!isDesktop()) {
      onToast("浏览器预览不会修改本地插件链接。");
      return;
    }
    setUnlinkingPluginId(plugin.id);
    try {
      await command<void>("unlink_plugin_from_local", { pluginId: plugin.id });
      await refreshPlugins();
      onToast(`已解除 ${plugin.name} 的本地链接；项目源文件未被修改。`);
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法解除本地插件链接。");
    } finally {
      setUnlinkingPluginId(null);
    }
  };

  const chooseDirectory = async (target: DirectoryPickerTarget) => {
    if (!isDesktop()) {
      onToast("浏览器预览不会打开本机文件夹选择器。");
      return;
    }

    setDirectoryPickerTarget(target);
    try {
      const selection = await command<SelectedDirectoryGrant | null>("select_directory");
      if (!selection) {
        return;
      }
      if (target === "rename") {
        setRenameDirectory(selection.path);
        setRenameDirectoryOpenId(selection.openId);
        setRenamePreview(null);
      } else if (target === "project") {
        setProjectParentDirectory(selection.path);
        setProjectParentDirectoryOpenId(selection.openId);
        setProjectResult(null);
      } else if (target === "local-plugin") {
        setLocalPluginDirectory(selection.path);
        setLocalPluginDirectoryOpenId(selection.openId);
      }
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法打开系统文件夹选择器。");
    } finally {
      setDirectoryPickerTarget(null);
    }
  };

  const clearRenamePreview = () => setRenamePreview(null);
  const developmentPlugins = plugins.filter((plugin) => plugin.isDevelopmentLink);

  return (
    <AnimatePresence>
      {open ? (
        <>
          <motion.button
            aria-label="关闭工具箱"
            className="drawer-scrim toolbox-scrim"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            onClick={closeToolbox}
            type="button"
          />
          <motion.aside
            aria-labelledby={activeTab === "search" ? "local-search-title" : activeTab === "json" ? "json-editor-title" : activeTab === "color" ? "color-workbench-title" : "toolbox-title"}
            aria-modal="true"
            className={`toolbox-drawer${activeTab === "search" ? " toolbox-drawer--search" : activeTab === "json" ? " toolbox-drawer--json" : activeTab === "color" ? " toolbox-drawer--color" : ""}`}
            initial={{ opacity: 0, y: 10, scale: 0.992 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 8, scale: 0.994 }}
            role="dialog"
            transition={{ duration: 0.18, ease: [0.16, 1, 0.3, 1] }}
          >
            {activeTab === "search" ? (
              <LocalSearchWorkspace
                indexStatus={indexStatus}
                isRefreshingIndex={isRefreshingIndex}
                onClose={closeToolbox}
                onOpenResult={onOpenSearchResult}
                onRefreshIndex={onRefreshIndex}
                onSetIndexRoots={onSetIndexRoots}
                onStartWindowDrag={onStartWindowDrag}
                onToast={onToast}
              />
            ) : activeTab === "json" ? (
              <JsonEditorWorkspace
                input={jsonInput}
                onClose={closeToolbox}
                onCopy={copyText}
                onInputChange={setJsonInput}
                onStartWindowDrag={onStartWindowDrag}
                onToast={onToast}
              />
            ) : activeTab === "color" ? (
              <ColorWorkbench
                color={color}
                onClose={closeToolbox}
                onColorChange={setColor}
                onCopy={copyText}
                onPickScreenColor={pickScreenColor}
                onStartWindowDrag={onStartWindowDrag}
                onToast={onToast}
              />
            ) : (
              <>
            <div className="drawer-header toolbox-drawer__header">
              <div>
                <p className="eyebrow">BUILT-IN WORKBENCH</p>
                <h2 id="toolbox-title">本地工具箱</h2>
              </div>
              <button aria-label="关闭工具箱" className="icon-button" onClick={closeToolbox} type="button">
                <X size={18} />
              </button>
            </div>

            <div aria-label="工具类别" className="toolbox-tabs" role="tablist">
              {tabs.map((tab) => {
                const Icon = tab.icon;
                const selected = activeTab === tab.id;
                return (
                  <button
                    aria-controls={`toolbox-panel-${tab.id}`}
                    aria-selected={selected}
                    className={"toolbox-tab" + (selected ? " is-active" : "")}
                    disabled={isRecordingSessionActive && !selected}
                    key={tab.id}
                    onClick={() => onTabChange(tab.id)}
                    role="tab"
                    type="button"
                  >
                    <Icon size={14} />
                    {tab.label}
                  </button>
                );
              })}
            </div>

            <div className="toolbox-content">
              {activeTab === "screenshot" ? (
                <section aria-labelledby="toolbox-screenshot-title" id="toolbox-panel-screenshot" role="tabpanel">
                  <div className="toolbox-section-heading">
                    <span className="toolbox-section-heading__icon"><Camera size={17} /></span>
                    <div>
                      <h3 id="toolbox-screenshot-title">截图</h3>
                      <p>读取一帧后，在预览中拖拽真正的矩形选区；确认前不会写入磁盘。</p>
                    </div>
                  </div>
                  <div className="toolbox-action-row toolbox-screenshot-actions">
                    {isDesktop() ? (
                      <button
                        className="toolbox-record-action"
                        disabled={isCapturingScreenshot}
                        onClick={() => void capturePrimaryMonitorNatively()}
                        type="button"
                      >
                        {isCapturingScreenshot ? <LoaderCircle className="spin" size={16} /> : <Camera size={16} />}
                        {isCapturingScreenshot ? "正在读取画面…" : "截取显示器后选择区域"}
                      </button>
                    ) : null}
                    <button
                      className="toolbox-secondary-action"
                      disabled={isCapturingScreenshot}
                      onClick={() => void captureScreenshot()}
                      type="button"
                    >
                      <Camera size={15} />
                      选择来源后框选区域
                    </button>
                    {import.meta.env.DEV && !isDesktop() ? (
                      <button
                        className="toolbox-secondary-action"
                        disabled={isCapturingScreenshot}
                        onClick={loadRegionCaptureDemo}
                        type="button"
                      >
                        <Crop size={15} />
                        载入模拟画面（开发验证）
                      </button>
                    ) : null}
                  </div>
                  {regionCaptureSource ? (
                    <RegionCaptureEditor
                      developmentPreview={!isDesktop()}
                      onCancel={() => {
                        replaceRegionCaptureSource(null);
                        onToast("已取消矩形截图。");
                      }}
                      onExport={exportRegionCapture}
                      onStatus={onToast}
                      source={regionCaptureSource}
                    />
                  ) : null}
                  {screenshotPreviewUrl ? (
                    <figure className="toolbox-screenshot-preview">
                      <img alt="截图或粘贴图片预览" src={screenshotPreviewUrl} />
                      <figcaption>{screenshotPreviewDescription}</figcaption>
                      <button
                        className="toolbox-secondary-action"
                        onClick={downloadScreenshotPreview}
                        type="button"
                      >
                        <Download size={15} />
                        下载图片
                      </button>
                    </figure>
                  ) : null}
                  <p className="toolbox-note">两种来源都只能由你点击触发：桌面端复用一次受限的原生显示器帧，浏览器来源复用系统共享选择器的一帧。拖拽完成后才在当前 WebView 裁剪 PNG；取消会丢弃帧，下载按钮才会保存。macOS 首次读取显示器可能要求“屏幕录制”权限。</p>
                </section>
              ) : null}

              {activeTab === "clipboard" ? (
                <section aria-labelledby="toolbox-clipboard-title" id="toolbox-panel-clipboard" role="tabpanel">
                  <div className="toolbox-section-heading">
                    <span className="toolbox-section-heading__icon"><Clipboard size={17} /></span>
                    <div>
                      <h3 id="toolbox-clipboard-title">剪贴板历史</h3>
                      <p>iHub 不上传内容；文本默认可选，图片与文件引用均需单独授权。</p>
                    </div>
                  </div>
                  {!isDesktop() ? (
                    <p className="toolbox-note">浏览器预览不会监听系统剪贴板。请在 iHub 桌面版中开启这项本地功能。</p>
                  ) : (
                    <>
                      <label className="toolbox-checkbox">
                        <input
                          checked={clipboardHistory?.enabled ?? false}
                          disabled={clipboardActionId === "history-enabled" || isLoadingClipboardHistory}
                          onChange={(event) => void setClipboardHistoryEnabled(event.target.checked)}
                          type="checkbox"
                        />
                        在本机记录剪贴板文本
                      </label>
                      <div aria-label="剪贴板历史格式设置" className="clipboard-history-options">
                        <label className="toolbox-checkbox">
                          <input
                            checked={clipboardHistory?.imageHistoryEnabled ?? false}
                            disabled={!clipboardHistory?.enabled || clipboardActionId === "history-options" || isLoadingClipboardHistory}
                            onChange={(event) => void setClipboardHistoryCaptureOptions(
                              event.target.checked,
                              clipboardHistory?.fileHistoryEnabled ?? false,
                            )}
                            type="checkbox"
                          />
                          <span>
                            <strong>记录图片（可选）</strong>
                            <small>最多 12 张；单张 PNG 最多 4 MB。预览或还原前不会把像素发到界面。</small>
                          </span>
                        </label>
                        <label className="toolbox-checkbox">
                          <input
                            checked={clipboardHistory?.fileHistoryEnabled ?? false}
                            disabled={!clipboardHistory?.enabled || clipboardActionId === "history-options" || isLoadingClipboardHistory}
                            onChange={(event) => void setClipboardHistoryCaptureOptions(
                              clipboardHistory?.imageHistoryEnabled ?? false,
                              event.target.checked,
                            )}
                            type="checkbox"
                          />
                          <span>
                            <strong>记录文件引用（可选）</strong>
                            <small>只保留受限名称/类型与原生私有校验信息，不读取或保存文件内容。</small>
                          </span>
                        </label>
                      </div>
                      <p className="toolbox-note">关闭总开关会停止所有采集并关闭额外格式；已有记录由你决定何时删除。文件复制或打开前，iHub 会在原生层重新验证路径、类型和文件指纹。已获 <code>clipboard.history</code> 权限的插件可按需读取文本历史；图片和文件引用不会交给插件。</p>
                      <div className="toolbox-action-row">
                        <button
                          className="toolbox-secondary-action"
                          disabled={isLoadingClipboardHistory}
                          onClick={() => void refreshClipboardHistory()}
                          type="button"
                        >
                          {isLoadingClipboardHistory ? <LoaderCircle className="spin" size={14} /> : <RefreshCw size={14} />}
                          刷新历史
                        </button>
                        {clipboardHistory?.enabled && clipboardHistory.items.some((item) => !item.pinned) ? (
                          <button
                            className="toolbox-danger-action"
                            disabled={clipboardActionId === "clear-unpinned"}
                            onClick={() => void clearUnpinnedClipboardHistory()}
                            type="button"
                          >
                            {clipboardActionId === "clear-unpinned" ? <LoaderCircle className="spin" size={14} /> : <Trash2 size={14} />}
                            清除未固定
                          </button>
                        ) : null}
                      </div>
                      {clipboardHistory?.enabled ? (
                        <div aria-label="剪贴板历史记录" className="clipboard-history-list">
                          {clipboardHistory.items.map((item) => {
                            const isBusy = clipboardActionId?.startsWith(`${item.id}:`) ?? false;
                            const imagePreview = clipboardImagePreview?.id === item.id
                              ? clipboardImagePreview.image
                              : null;
                            return (
                              <article
                                className={"clipboard-history-item clipboard-history-item--" + item.kind + (item.pinned ? " is-pinned" : "")}
                                key={item.id}
                              >
                                <header className="clipboard-history-item__kind">
                                  <span>{clipboardHistoryKindLabel(item.kind)}</span>
                                  {item.kind === "image" && item.image ? (
                                    <small>{item.image.width} × {item.image.height} · {formatClipboardHistoryBytes(item.image.byteLength)}</small>
                                  ) : null}
                                  {item.kind === "files" ? <small>{item.files.length} 项 · 路径不显示</small> : null}
                                </header>
                                {item.kind === "text" ? <p title={item.text}>{item.text}</p> : null}
                                {item.kind === "image" ? (
                                  <div className="clipboard-history-image">
                                    {imagePreview ? (
                                      <figure className="clipboard-history-image__preview">
                                        <img alt="按需载入的剪贴板历史图片预览" src={imagePreview.dataUrl} />
                                        <figcaption>仅当前会话预览；不会自动导出或上传。</figcaption>
                                      </figure>
                                    ) : (
                                      <p>图片像素未载入界面。点击预览或还原图片才会读取本地 PNG。</p>
                                    )}
                                  </div>
                                ) : null}
                                {item.kind === "files" ? (
                                  <ul className="clipboard-history-files" aria-label="已保存的文件引用">
                                    {item.files.map((file, fileIndex) => (
                                      <li key={`${item.id}:${fileIndex}`}>
                                        <span title={file.name}>{file.name} <small>{file.kind === "folder" ? "文件夹" : "文件"}</small></span>
                                        <button
                                          className="toolbox-icon-action"
                                          disabled={isBusy}
                                          onClick={() => void openClipboardHistoryFileEntry(item, fileIndex)}
                                          title="在系统中打开（会先重新验证）"
                                          type="button"
                                        >
                                          {clipboardActionId === `${item.id}:open:${fileIndex}` ? <LoaderCircle className="spin" size={13} /> : <ArrowRight size={13} />}
                                        </button>
                                      </li>
                                    ))}
                                  </ul>
                                ) : null}
                                <footer>
                                  <time dateTime={item.capturedAt}>{formatClipboardTime(item.capturedAt)}</time>
                                  <div>
                                    {item.kind === "image" ? (
                                      <button
                                        aria-label="预览这张剪贴板历史图片"
                                        className="toolbox-icon-action"
                                        disabled={isBusy}
                                        onClick={() => void previewClipboardHistoryImage(item)}
                                        title="按需预览"
                                        type="button"
                                      >
                                        {clipboardActionId === `${item.id}:preview` ? <LoaderCircle className="spin" size={14} /> : <Camera size={14} />}
                                      </button>
                                    ) : null}
                                    <button
                                      aria-label={clipboardHistoryRestoreLabel(item.kind)}
                                      className="toolbox-icon-action"
                                      disabled={isBusy}
                                      onClick={() => void restoreClipboardHistoryItem(item)}
                                      title={clipboardHistoryRestoreLabel(item.kind)}
                                      type="button"
                                    >
                                      {isBusy ? <LoaderCircle className="spin" size={14} /> : <Copy size={14} />}
                                    </button>
                                    <button
                                      aria-label={item.pinned ? "取消固定" : "固定这条剪贴板内容"}
                                      className="toolbox-icon-action"
                                      disabled={isBusy}
                                      onClick={() => void setClipboardHistoryPinned(item.id, !item.pinned)}
                                      title={item.pinned ? "取消固定" : "固定"}
                                      type="button"
                                    >
                                      {item.pinned ? <PinOff size={14} /> : <Pin size={14} />}
                                    </button>
                                    <button
                                      aria-label="删除这条剪贴板内容"
                                      className="toolbox-icon-action"
                                      disabled={isBusy}
                                      onClick={() => void deleteClipboardHistoryItem(item.id)}
                                      title="删除"
                                      type="button"
                                    >
                                      <Trash2 size={14} />
                                    </button>
                                  </div>
                                </footer>
                              </article>
                            );
                          })}
                          {!clipboardHistory.items.length ? (
                            <p className="toolbox-note">尚无记录。启用的格式会在本机后台加入此列表；图片与文件引用仍需各自的开关。</p>
                          ) : null}
                        </div>
                      ) : (
                        <div className="clipboard-history-empty">
                          <Clipboard size={19} />
                          <strong>隐私优先，尚未开始记录</strong>
                          <span>打开上方开关后，iHub 才会在这台设备上保存新的纯文本记录；图片与文件引用必须再次单独启用。</span>
                        </div>
                      )}
                    </>
                  )}
                </section>
              ) : null}

              {activeTab === "markdown" ? <MarkdownWorkbench onToast={onToast} /> : null}

              {activeTab === "note" ? (
                <section aria-labelledby="toolbox-note-title" id="toolbox-panel-note" role="tabpanel">
                  <div className="toolbox-section-heading">
                    <span className="toolbox-section-heading__icon"><NotebookPen size={17} /></span>
                    <div>
                      <h3 id="toolbox-note-title">快速便签</h3>
                      <p>仅保存在此设备的本地存储中；支持搜索、复制与删除。</p>
                    </div>
                  </div>
                  <textarea
                    aria-label="新建便签"
                    className="toolbox-code-input"
                    onChange={(event) => setQuickNoteDraft(event.target.value)}
                    placeholder="写下一条不想丢的内容…"
                    spellCheck="true"
                    value={quickNoteDraft}
                  />
                  <div className="toolbox-action-row">
                    <button
                      className="accent-button toolbox-primary-action"
                      onClick={saveQuickNote}
                      type="button"
                    >
                      <Plus size={15} />
                      保存便签
                    </button>
                    {quickNoteDraft ? (
                      <button
                        className="toolbox-secondary-action"
                        onClick={() => setQuickNoteDraft("")}
                        type="button"
                      >
                        清空输入
                      </button>
                    ) : null}
                  </div>
                  <label className="toolbox-field">
                    <span>搜索已保存便签</span>
                    <input
                      onChange={(event) => setQuickNoteQuery(event.target.value)}
                      placeholder="按内容筛选…"
                      value={quickNoteQuery}
                    />
                  </label>
                  <div className="toolbox-statline">
                    <span>LOCAL NOTES</span>
                    <strong>{filteredQuickNotes.length} / {quickNotes.length} 条</strong>
                  </div>
                  <div className="rename-preview" aria-label="已保存的便签">
                    {filteredQuickNotes.map((note) => (
                      <div key={note.id}>
                        <div className="rename-preview__item" title={note.text}>
                          <span>{formatNoteTime(note.updatedAt)}</span>
                          <ArrowRight size={13} />
                          <strong>{notePreview(note.text)}</strong>
                        </div>
                        <div className="toolbox-action-row">
                          <button
                            className="toolbox-secondary-action"
                            onClick={() => void copyText(note.text, "便签")}
                            type="button"
                          >
                            <Copy size={14} />
                            复制
                          </button>
                          <button
                            className="toolbox-danger-action"
                            onClick={() => deleteQuickNote(note)}
                            type="button"
                          >
                            <Trash2 size={14} />
                            删除
                          </button>
                        </div>
                      </div>
                    ))}
                    {!filteredQuickNotes.length ? (
                      <p className="toolbox-note">
                        {quickNotes.length ? "没有匹配的便签。" : "还没有便签；保存后会一直留在这台设备上。"}
                      </p>
                    ) : null}
                  </div>
                </section>
              ) : null}

              {activeTab === "convert" ? (
                <section aria-labelledby="toolbox-convert-title" id="toolbox-panel-convert" role="tabpanel">
                  <div className="toolbox-section-heading">
                    <span className="toolbox-section-heading__icon"><Binary size={17} /></span>
                    <div>
                      <h3 id="toolbox-convert-title">进制与文本转换</h3>
                      <p>数值使用 BigInt 转换，不会因 JavaScript Number 精度限制而截断。</p>
                    </div>
                  </div>
                  <label className="toolbox-field">
                    <span>整数输入</span>
                    <input
                      aria-label={`${conversionBase} 进制整数输入`}
                      onChange={(event) => setConversionInput(event.target.value)}
                      placeholder="例如 2026、7EA 或 0x7EA"
                      spellCheck="false"
                      value={conversionInput}
                    />
                  </label>
                  <div aria-label="输入进制" className="toolbox-tabs" role="tablist">
                    {numberBases.map((entry) => (
                      <button
                        aria-selected={conversionBase === entry.base}
                        className={"toolbox-tab" + (conversionBase === entry.base ? " is-active" : "")}
                        key={entry.base}
                        onClick={() => setConversionBase(entry.base)}
                        role="tab"
                        type="button"
                      >
                        {entry.label} {entry.base}
                      </button>
                    ))}
                  </div>
                  {numberConversion.valid ? (
                    numberConversion.values?.length ? (
                      <div className="color-values">
                        {numberConversion.values.map((entry) => (
                          <button
                            className="color-value"
                            key={entry.base}
                            onClick={() => void copyText(entry.value, `${entry.label} 数值`)}
                            type="button"
                          >
                            <span>{entry.label}</span>
                            <strong>{entry.value}</strong>
                            <Copy size={14} />
                          </button>
                        ))}
                      </div>
                    ) : (
                      <p className="toolbox-note">输入一个二、八、十或十六进制整数，即可查看全部结果。</p>
                    )
                  ) : (
                    <p className="toolbox-feedback is-error" role="status">
                      <CircleAlert size={14} />
                      {numberConversion.error}
                    </p>
                  )}

                  <div className="local-plugin-linker">
                    <div className="local-plugin-linker__heading">
                      <strong>UTF-8 文本编码</strong>
                      <span>OFFLINE</span>
                    </div>
                    <p>可在文本与 UTF-8 Hex / Base64 之间双向转换；不会上传任何内容。</p>
                    <div aria-label="文本转换方向" className="toolbox-tabs" role="tablist">
                      {([
                        ["encode", "文本 → 编码"],
                        ["decode", "编码 → 文本"],
                      ] as const).map(([direction, label]) => (
                        <button
                          aria-selected={textConversionDirection === direction}
                          className={"toolbox-tab" + (textConversionDirection === direction ? " is-active" : "")}
                          key={direction}
                          onClick={() => setTextConversionDirection(direction)}
                          role="tab"
                          type="button"
                        >
                          {label}
                        </button>
                      ))}
                    </div>
                    <div aria-label="文本编码格式" className="toolbox-tabs" role="tablist">
                      {([
                        ["hex", "UTF-8 HEX"],
                        ["base64", "BASE64"],
                      ] as const).map(([encoding, label]) => (
                        <button
                          aria-selected={textEncoding === encoding}
                          className={"toolbox-tab" + (textEncoding === encoding ? " is-active" : "")}
                          key={encoding}
                          onClick={() => setTextEncoding(encoding)}
                          role="tab"
                          type="button"
                        >
                          {label}
                        </button>
                      ))}
                    </div>
                    <label className="toolbox-field">
                      <span>{textConversionDirection === "encode" ? "待编码文本" : "待解码内容"}</span>
                      <textarea
                        aria-label={textConversionDirection === "encode" ? "待编码文本" : "待解码内容"}
                        className="toolbox-code-input"
                        onChange={(event) => setTextConversionInput(event.target.value)}
                        placeholder={textConversionDirection === "encode"
                          ? "输入任意文本"
                          : textEncoding === "hex" ? "E4 B8 AD E6 96 87" : "5Lit5paH"}
                        spellCheck="false"
                        value={textConversionInput}
                      />
                    </label>
                    {textConversion.valid ? (
                      <>
                        <label className="toolbox-field">
                          <span>转换结果</span>
                          <textarea
                            aria-label="文本转换结果"
                            className="toolbox-code-input"
                            readOnly
                            spellCheck="false"
                            value={textConversion.value ?? ""}
                          />
                        </label>
                        <div className="toolbox-action-row">
                          <button
                            className="toolbox-secondary-action"
                            disabled={!textConversion.value}
                            onClick={() => void copyText(textConversion.value ?? "", "转换结果")}
                            type="button"
                          >
                            <Copy size={14} />
                            复制结果
                          </button>
                        </div>
                      </>
                    ) : (
                      <p className="toolbox-feedback is-error" role="status">
                        <CircleAlert size={14} />
                        {textConversion.error}
                      </p>
                    )}
                  </div>
                </section>
              ) : null}

              {activeTab === "calculator" ? (
                <section aria-labelledby="toolbox-calculator-title" id="toolbox-panel-calculator" role="tabpanel">
                  <div className="toolbox-section-heading">
                    <span className="toolbox-section-heading__icon"><Calculator size={17} /></span>
                    <div>
                      <h3 id="toolbox-calculator-title">离线计算器</h3>
                      <p>表达式只在当前设备解析，支持括号、百分号、幂、小数和科学计数法。</p>
                    </div>
                  </div>
                  <label className="toolbox-field" htmlFor="calculator-input">
                    <span>表达式</span>
                    <input
                      aria-describedby="calculator-hint"
                      className="toolbox-calculator-input"
                      id="calculator-input"
                      inputMode="decimal"
                      onChange={(event) => setCalculatorInput(event.target.value)}
                      onKeyDown={(event) => {
                        if (event.nativeEvent.isComposing) {
                          return;
                        }
                        if (event.key === "Enter") {
                          event.preventDefault();
                          commitCalculator();
                        }
                      }}
                      placeholder="例如 (512 + 256) / 3"
                      ref={calculatorInputRef}
                      spellCheck="false"
                      value={calculatorInput}
                    />
                  </label>
                  <p className="toolbox-note" id="calculator-hint">使用 <code>^</code> 表示幂，<code>%</code> 表示取余；按 Enter 或等号保存本次结果。</p>
                  {calculatorEvaluation.valid ? (
                    <div aria-live="polite" className="calculator-result">
                      <span>RESULT</span>
                      <strong>{calculatorEvaluation.formatted ?? "—"}</strong>
                      <button
                        aria-label="复制计算结果"
                        className="toolbox-icon-action"
                        disabled={!calculatorEvaluation.formatted}
                        onClick={() => void copyText(calculatorEvaluation.formatted ?? "", "计算结果")}
                        title="复制结果"
                        type="button"
                      >
                        <Copy size={15} />
                      </button>
                    </div>
                  ) : (
                    <p className="toolbox-feedback is-error" role="status">
                      <CircleAlert size={14} />
                      {calculatorEvaluation.error}
                    </p>
                  )}
                  <div aria-label="计算器按键" className="calculator-pad" role="group">
                    {calculatorKeys.map((key) => (
                      <button
                        className={"calculator-key" + (key === "=" ? " is-equals" : "") + (key === "⌫" ? " is-utility" : "")}
                        key={key}
                        onClick={() => appendCalculatorKey(key)}
                        type="button"
                      >
                        {key}
                      </button>
                    ))}
                  </div>
                  <div className="toolbox-action-row">
                    <button className="accent-button toolbox-primary-action" onClick={commitCalculator} type="button">
                      <Calculator size={15} />
                      计算并保存
                    </button>
                    <button className="toolbox-secondary-action" onClick={() => setCalculatorInput("")} type="button">
                      清空表达式
                    </button>
                  </div>
                  <div className="local-plugin-linker">
                    <div className="local-plugin-linker__heading">
                      <strong>本机计算历史</strong>
                      <span>{calculatorHistory.length} 条</span>
                    </div>
                    {calculatorHistory.length ? (
                      <div className="calculator-history" aria-label="计算历史">
                        {calculatorHistory.map((entry) => (
                          <button
                            className="calculator-history__item"
                            key={entry.id}
                            onClick={() => setCalculatorInput(entry.expression)}
                            title="载入此表达式"
                            type="button"
                          >
                            <code>{entry.expression}</code>
                            <ArrowRight size={13} />
                            <strong>{entry.result}</strong>
                          </button>
                        ))}
                      </div>
                    ) : (
                      <p>还没有历史记录；计算后会仅保存在此设备的本地存储中。</p>
                    )}
                  </div>
                </section>
              ) : null}

              {activeTab === "time" ? (
                <section aria-labelledby="toolbox-time-title" id="toolbox-panel-time" role="tabpanel">
                  <div className="toolbox-section-heading">
                    <span className="toolbox-section-heading__icon"><Clock3 size={17} /></span>
                    <div>
                      <h3 id="toolbox-time-title">时间与时间戳</h3>
                      <p>离线转换 Unix 秒、毫秒和日期文本，并按本机、UTC 或指定 IANA 时区查看。</p>
                    </div>
                  </div>

                  <div aria-live="off" className="time-live-strip">
                    <span className="time-live-strip__pulse" />
                    <div>
                      <span>当前时间 · {localTimeZone}</span>
                      <strong>{currentTimeSnapshot.ok ? currentTimeSnapshot.local.value : "暂不可用"}</strong>
                      <code>{currentTimeSnapshot.ok ? currentTimeSnapshot.epochSeconds : "—"}</code>
                    </div>
                    <button
                      aria-label="立即刷新当前时间"
                      className="toolbox-icon-action"
                      onClick={() => setCurrentTimeMilliseconds(Date.now())}
                      title="立即刷新"
                      type="button"
                    >
                      <RefreshCw size={14} />
                    </button>
                  </div>

                  <label className="toolbox-field" htmlFor="time-input">
                    <span>时间戳或日期文本</span>
                    <input
                      aria-describedby="time-input-hint"
                      autoComplete="off"
                      id="time-input"
                      onChange={(event) => setTimeInput(event.target.value)}
                      placeholder="1700000000、1700000000000 或 2024-01-02T03:04:05Z"
                      ref={timeInputRef}
                      spellCheck="false"
                      value={timeInput}
                    />
                  </label>
                  <div className="toolbox-action-row">
                    <button
                      className="toolbox-secondary-action"
                      onClick={() => {
                        const now = Date.now();
                        setCurrentTimeMilliseconds(now);
                        setTimeInput(now.toString());
                      }}
                      type="button"
                    >
                      <Clock3 size={14} />
                      使用当前时间
                    </button>
                    <button
                      className="toolbox-secondary-action"
                      onClick={() => setTimeInput(new Date().toISOString())}
                      type="button"
                    >
                      ISO 示例
                    </button>
                  </div>
                  <p className="toolbox-note" id="time-input-hint">
                    10 位整数按秒、13 位整数按毫秒；11 位请加 <code>s</code> 或 <code>ms</code>。无时区的 <code>YYYY-MM-DD HH:mm:ss</code> 按本机时区解析，ISO 文本需带 <code>Z</code> 或明确偏移。
                  </p>

                  {parsedTime.ok ? (
                    <p className="toolbox-feedback is-success" role="status">
                      <Check size={14} />
                      {timeInputKindLabel(parsedTime.inputKind)}
                    </p>
                  ) : (
                    <p className="toolbox-feedback is-error" role="status">
                      <CircleAlert size={14} />
                      {parsedTime.error}
                    </p>
                  )}

                  <label className="toolbox-field" htmlFor="time-zone-input">
                    <span>对照 IANA 时区</span>
                    <input
                      autoComplete="off"
                      id="time-zone-input"
                      list="time-zone-suggestions"
                      onChange={(event) => setSelectedTimeZone(event.target.value)}
                      placeholder="例如 America/New_York"
                      spellCheck="false"
                      value={selectedTimeZone}
                    />
                  </label>
                  <datalist id="time-zone-suggestions">
                    {commonTimeZones.map((timeZone) => <option key={timeZone} value={timeZone} />)}
                  </datalist>

                  {timeSnapshot?.ok ? (
                    <>
                      <div aria-label="时间转换结果" className="time-value-list">
                        {[
                          ["UNIX 秒", timeSnapshot.epochSeconds],
                          ["UNIX 毫秒", timeSnapshot.epochMilliseconds],
                          [`本机 · ${localTimeZone}`, timeSnapshot.local.value],
                          ["UTC", timeSnapshot.utc.value],
                          ["ISO 8601", timeSnapshot.iso],
                          ...(timeSnapshot.selected
                            ? [[selectedTimeZone, timeSnapshot.selected.value]]
                            : []),
                        ].map(([label, value], index) => (
                          <button
                            className="time-value-row"
                            key={`${label}:${index}`}
                            onClick={() => void copyText(value, label)}
                            title={`复制 ${label}`}
                            type="button"
                          >
                            <span>{label}</span>
                            <strong>{value}</strong>
                            <Copy size={14} />
                          </button>
                        ))}
                      </div>
                      {timeSnapshot.selectedError ? (
                        <p className="toolbox-feedback is-error" role="status">
                          <CircleAlert size={14} />
                          {timeSnapshot.selectedError}
                        </p>
                      ) : null}
                    </>
                  ) : timeSnapshot ? (
                    <p className="toolbox-feedback is-error" role="status">
                      <CircleAlert size={14} />
                      {timeSnapshot.error}
                    </p>
                  ) : null}
                  <p className="toolbox-note">当前时间仅在这个工具可见时每秒刷新；转换和时区计算全部在本机完成。夏令时规则由系统内置的 <code>Intl</code> 时区数据库提供。</p>
                </section>
              ) : null}

              {activeTab === "qrcode" ? (
                <section aria-labelledby="toolbox-qrcode-title" id="toolbox-panel-qrcode" role="tabpanel">
                  <div className="toolbox-section-heading">
                    <span className="toolbox-section-heading__icon"><QrCode size={17} /></span>
                    <div>
                      <h3 id="toolbox-qrcode-title">二维码</h3>
                      <p>离线生成文本或 URL 二维码，也可识别你主动选择的本地图片；内容不会发送到网络。</p>
                    </div>
                  </div>
                  <textarea
                    aria-label="二维码内容"
                    className="toolbox-code-input"
                    onChange={(event) => {
                      setQrInput(event.target.value);
                      setQrError(null);
                      setQrPreviewUrl(null);
                    }}
                    placeholder="输入 URL、文本或任意可扫码内容…"
                    spellCheck="false"
                    value={qrInput}
                  />
                  <div className="toolbox-action-row">
                    <button
                      className="accent-button toolbox-primary-action"
                      disabled={isGeneratingQr}
                      onClick={() => void generateQrCode()}
                      type="button"
                    >
                      {isGeneratingQr ? <LoaderCircle className="spin" size={15} /> : <QrCode size={15} />}
                      {isGeneratingQr ? "正在生成…" : "生成二维码"}
                    </button>
                    <button
                      className="toolbox-secondary-action"
                      disabled={!qrInput}
                      onClick={() => void copyText(qrInput, "二维码原文")}
                      type="button"
                    >
                      <Copy size={14} />
                      复制原文
                    </button>
                  </div>
                  {qrError ? (
                    <p className="toolbox-feedback is-error" role="status">
                      <CircleAlert size={14} />
                      {qrError}
                    </p>
                  ) : null}
                  {qrPreviewUrl ? (
                    <>
                      <figure className="toolbox-screenshot-preview">
                        <img alt="已生成的二维码预览" src={qrPreviewUrl} />
                        <figcaption>扫码内容与上方原文一致。PNG 已在本地生成，可直接保存。</figcaption>
                      </figure>
                      <div className="toolbox-action-row">
                        <button
                          className="toolbox-secondary-action"
                          onClick={downloadQrCode}
                          type="button"
                        >
                          <Download size={14} />
                          下载 PNG
                        </button>
                      </div>
                    </>
                  ) : (
                    <p className="toolbox-note">支持短文本、链接和 Unicode 内容。生成完成后可预览并下载 PNG。</p>
                  )}
                  <div className="local-plugin-linker qr-decode-panel">
                    <div className="local-plugin-linker__heading">
                      <strong>识别图片中的二维码</strong>
                      <span>OFFLINE</span>
                    </div>
                    <p>图片只在当前 WebView 解码；不会上传、不会读取相册或调用摄像头。</p>
                    <input
                      accept="image/png,image/jpeg,image/webp,image/gif,image/bmp"
                      aria-label="选择要识别的二维码图片"
                      hidden
                      onChange={(event) => {
                        const file = event.currentTarget.files?.[0];
                        event.currentTarget.value = "";
                        if (file) {
                          void decodeQrImage(file);
                        }
                      }}
                      ref={qrDecodeInputRef}
                      type="file"
                    />
                    <div className="toolbox-action-row qr-decode-panel__actions">
                      <button
                        className="toolbox-secondary-action"
                        disabled={isDecodingQr}
                        onClick={() => qrDecodeInputRef.current?.click()}
                        type="button"
                      >
                        {isDecodingQr ? <LoaderCircle className="spin" size={14} /> : <FolderSearch size={14} />}
                        {isDecodingQr ? "正在识别…" : "选择图片并识别"}
                      </button>
                      {qrDecodeFileName ? <span className="qr-decode-panel__file">{qrDecodeFileName}</span> : null}
                    </div>
                    {qrDecodeError ? (
                      <p className="toolbox-feedback is-error" role="status">
                        <CircleAlert size={14} />
                        {qrDecodeError}
                      </p>
                    ) : null}
                    {qrDecodeValue ? (
                      <div className="qr-decode-panel__result" role="status">
                        <span>识别结果</span>
                        <code>{qrDecodeValue}</code>
                        <div className="toolbox-action-row">
                          <button
                            className="toolbox-secondary-action"
                            onClick={() => void copyText(qrDecodeValue, "二维码识别结果")}
                            type="button"
                          >
                            <Copy size={14} />
                            复制结果
                          </button>
                          <button
                            className="toolbox-secondary-action"
                            onClick={() => {
                              setQrInput(qrDecodeValue);
                              setQrPreviewUrl(null);
                              setQrError(null);
                              onToast("已把识别结果填入生成器；如需生成新二维码，请点击“生成二维码”。");
                            }}
                            type="button"
                          >
                            <ArrowRight size={14} />
                            填入生成器
                          </button>
                        </div>
                      </div>
                    ) : null}
                  </div>
                </section>
              ) : null}

              {activeTab === "cloud" ? (
                <section aria-labelledby="toolbox-cloud-title" id="toolbox-panel-cloud" role="tabpanel">
                  <div className="toolbox-section-heading">
                    <span className="toolbox-section-heading__icon"><Cloud size={17} /></span>
                    <div>
                      <h3 id="toolbox-cloud-title">云盘</h3>
                      <p>统一连接与文件工作面；WebDAV 已可用，OAuth 云盘按同一原生适配器契约接入。</p>
                    </div>
                  </div>
                  {!isDesktop() ? (
                    <p className="toolbox-note">浏览器预览不能发起受限的原生 WebDAV 请求。请在 iHub 桌面版中连接你的云盘。</p>
                  ) : (
                    <>
                      <div className="cloud-drive-profiles">
                        <div className="local-plugin-linker__heading">
                          <strong>已保存连接</strong>
                          <span>{isLoadingCloudProfiles ? "LOADING" : `${cloudProfiles.length} PROFILES`}</span>
                        </div>
                        {cloudProfiles.length ? (
                          <div className="cloud-drive-profile-list">
                            {cloudProfiles.map((profile) => {
                              const isConnected = webDavConnectedProfileId === profile.id && Boolean(webDavConnectionId);
                              const isConnecting = connectingCloudProfileId === profile.id;
                              const isForgetting = forgettingCloudProfileId === profile.id;
                              return (
                                <div className={`cloud-drive-profile${isConnected ? " is-connected" : ""}`} key={profile.id}>
                                  <span className="cloud-drive-profile__icon"><Cloud size={15} /></span>
                                  <div>
                                    <strong>{profile.label}</strong>
                                    <small title={profile.endpoint}>{profile.username ? `${profile.username} · ` : ""}{profile.endpoint}</small>
                                  </div>
                                  <button
                                    className="toolbox-secondary-action"
                                    disabled={Boolean(webDavConnectionId) || isLoadingWebDav || isForgetting}
                                    onClick={() => void connectSavedCloudProfile(profile)}
                                    type="button"
                                  >
                                    {isConnecting ? <LoaderCircle className="spin" size={13} /> : <ArrowRight size={13} />}
                                    {isConnected ? "已连接" : isConnecting ? "连接中…" : "连接"}
                                  </button>
                                  <button
                                    aria-label={`忘记 ${profile.label}`}
                                    className="cloud-drive-profile__forget"
                                    disabled={isLoadingWebDav || isUploadingWebDav || webDavDownloadingHref !== null || isForgetting}
                                    onClick={() => void forgetCloudProfile(profile)}
                                    title="从系统凭据库忘记此连接"
                                    type="button"
                                  >
                                    {isForgetting ? <LoaderCircle className="spin" size={13} /> : <Trash2 size={13} />}
                                  </button>
                                </div>
                              );
                            })}
                          </div>
                        ) : (
                          <p className="cloud-drive-profiles__empty">
                            {isLoadingCloudProfiles ? "正在读取本机连接…" : "还没有保存连接；你也可以只连接一次而不保存。"}
                          </p>
                        )}
                      </div>
                      <div className="cloud-drive-connection">
                        <div className="local-plugin-linker__heading">
                          <strong>{webDavConnectionId ? "当前连接" : "连接 WebDAV"}</strong>
                          <span>{webDavConnectionId ? "NATIVE SESSION" : "HTTPS · BASIC AUTH"}</span>
                        </div>
                        <label className="toolbox-field">
                          <span>WebDAV 根地址</span>
                          <input
                            autoComplete="off"
                            disabled={Boolean(webDavConnectionId) || isLoadingWebDav}
                            onChange={(event) => {
                              setWebDavEndpoint(event.target.value);
                              setWebDavError(null);
                            }}
                            placeholder="https://dav.example.com/remote.php/dav/files/name/"
                            spellCheck="false"
                            value={webDavEndpoint}
                          />
                        </label>
                        {!webDavConnectionId ? (
                          <div className="cloud-drive-connection__credentials">
                            <label className="toolbox-field">
                              <span>账号（可留空）</span>
                              <input
                                autoComplete="off"
                                disabled={isLoadingWebDav}
                                onChange={(event) => {
                                  setWebDavUsername(event.target.value);
                                  setWebDavError(null);
                                }}
                                placeholder="用户名或邮箱"
                                spellCheck="false"
                                value={webDavUsername}
                              />
                            </label>
                            <label className="toolbox-field">
                              <span>密码 / 应用专用密码</span>
                              <input
                                autoComplete="new-password"
                                disabled={isLoadingWebDav}
                                onChange={(event) => {
                                  setWebDavPassword(event.target.value);
                                  setWebDavError(null);
                                }}
                                placeholder={webDavRemember ? "连接成功后写入系统凭据库" : "仅用于这一次连接"}
                                type="password"
                                value={webDavPassword}
                              />
                            </label>
                          </div>
                        ) : (
                          <div className="cloud-drive-session-security">
                            <span><Check size={14} /></span>
                            <div>
                              <strong>密码已从前端清除</strong>
                              <small>{webDavUsername ? `${webDavUsername} · ` : ""}后续文件操作只使用原生连接 ID。</small>
                            </div>
                          </div>
                        )}
                        {!webDavConnectionId ? (
                          <>
                            <label className="toolbox-checkbox cloud-drive-remember">
                              <input
                                checked={webDavRemember}
                                disabled={isLoadingWebDav}
                                onChange={(event) => {
                                  setWebDavRemember(event.target.checked);
                                  setWebDavError(null);
                                }}
                                type="checkbox"
                              />
                              <span>记住到 Windows 凭据管理器 / macOS 钥匙串；iHub 元数据文件不保存密码。</span>
                            </label>
                            {webDavRemember ? (
                              <label className="toolbox-field cloud-drive-profile-label">
                                <span>连接名称（可留空）</span>
                                <input
                                  autoComplete="off"
                                  disabled={isLoadingWebDav}
                                  maxLength={96}
                                  onChange={(event) => setWebDavProfileLabel(event.target.value)}
                                  placeholder="例如：家庭 NAS"
                                  value={webDavProfileLabel}
                                />
                              </label>
                            ) : null}
                          </>
                        ) : null}
                        <div className="toolbox-action-row">
                          {!webDavConnectionId ? (
                            <button
                              className="accent-button toolbox-primary-action"
                              disabled={isLoadingWebDav || !webDavEndpoint.trim()}
                              onClick={() => void connectWebDav()}
                              type="button"
                            >
                              {isLoadingWebDav ? <LoaderCircle className="spin" size={15} /> : <Cloud size={15} />}
                              {isLoadingWebDav ? "正在连接…" : "连接并浏览"}
                            </button>
                          ) : (
                            <>
                              <button
                                className="toolbox-secondary-action"
                                disabled={!webDavLocation?.canGoUp || isLoadingWebDav || isUploadingWebDav || webDavDownloadingHref !== null}
                                onClick={openWebDavParent}
                                type="button"
                              >
                                <ArrowRight className="cloud-drive__back-icon" size={14} />
                                返回上级
                              </button>
                              <button
                                className="toolbox-secondary-action"
                                disabled={isLoadingWebDav || isUploadingWebDav || webDavDownloadingHref !== null}
                                onClick={() => void loadWebDavDirectory(webDavDirectory ?? undefined)}
                                type="button"
                              >
                                {isLoadingWebDav ? <LoaderCircle className="spin" size={14} /> : <RefreshCw size={14} />}
                                刷新目录
                              </button>
                              <button
                                className="toolbox-secondary-action"
                                disabled={isLoadingWebDav || isUploadingWebDav || webDavDownloadingHref !== null}
                                onClick={() => void uploadWebDavFile()}
                                type="button"
                              >
                                {isUploadingWebDav ? <LoaderCircle className="spin" size={14} /> : <Plus size={14} />}
                                {isUploadingWebDav ? "正在上传…" : "上传文件"}
                              </button>
                              <button className="toolbox-danger-action" disabled={isLoadingWebDav || isUploadingWebDav || webDavDownloadingHref !== null} onClick={() => void disconnectWebDav()} type="button">
                                <X size={14} />
                                断开
                              </button>
                            </>
                          )}
                        </div>
                      </div>
                      {webDavError ? (
                        <p className="toolbox-feedback is-error" role="status">
                          <CircleAlert size={14} />
                          {webDavError}
                        </p>
                      ) : null}
                      {webDavConnectionId && webDavConnectedRoot && webDavDirectory ? (
                        <div aria-label="WebDAV 目录内容" className="cloud-drive-directory">
                          <div className="local-plugin-linker__heading cloud-drive-directory__heading">
                            <strong>远端目录</strong>
                            <span>{webDavEntries.length} ITEMS</span>
                          </div>
                          <code className="cloud-drive-directory__path" title={webDavDirectory}>{webDavLocation?.path ?? webDavDirectory}</code>
                          {webDavEntries.length ? (
                            <div className="cloud-drive-directory__entries">
                              {webDavEntries.map((entry) => (
                                entry.isCollection ? (
                                  <button
                                    className="cloud-drive-entry cloud-drive-entry--folder"
                                    disabled={isLoadingWebDav || isUploadingWebDav || webDavDownloadingHref !== null}
                                    key={entry.href}
                                    onClick={() => void loadWebDavDirectory(entry.href)}
                                    type="button"
                                  >
                                    <FolderSearch size={16} />
                                    <span>
                                      <strong>{entry.name}</strong>
                                      <small>{entry.lastModified ?? "文件夹"}</small>
                                    </span>
                                    <ArrowRight size={14} />
                                  </button>
                                ) : (
                                  <div className="cloud-drive-entry" key={entry.href}>
                                    <Files size={16} />
                                    <span>
                                      <strong>{entry.name}</strong>
                                      <small>{formatWebDavBytes(entry.contentLength)}{entry.contentType ? ` · ${entry.contentType}` : ""}</small>
                                    </span>
                                    <button
                                      className="toolbox-secondary-action cloud-drive-entry__download"
                                      disabled={isLoadingWebDav || isUploadingWebDav || webDavDownloadingHref !== null}
                                      onClick={() => void downloadWebDavFile(entry)}
                                      type="button"
                                    >
                                      {webDavDownloadingHref === entry.href ? <LoaderCircle className="spin" size={13} /> : <Download size={13} />}
                                      {webDavDownloadingHref === entry.href ? "下载中…" : "下载"}
                                    </button>
                                  </div>
                                )
                              ))}
                            </div>
                          ) : (
                            <p className="cloud-drive-directory__empty">这个目录为空，或服务没有返回可显示的同源项目。</p>
                          )}
                        </div>
                      ) : null}
                      <div className="local-plugin-linker cloud-drive-adapters">
                        <div className="local-plugin-linker__heading">
                          <strong>云服务适配器</strong>
                          <span>SAFE ROADMAP</span>
                        </div>
                        <p>所有服务都复用这一套“连接、目录浏览、下载、上传、文件操作”工作面；不会嵌入各云盘的网站或给不同服务做不同 UI。</p>
                        <div aria-label="云服务适配器状态" className="cloud-drive-provider-list">
                          {cloudDriveProviders.map((provider) => (
                            <div className={`cloud-drive-provider is-${provider.status}`} key={provider.id}>
                              <div>
                                <strong>{provider.name}</strong>
                                <small>{provider.description}</small>
                              </div>
                              <span>{provider.status === "available" ? "READY" : "OAUTH ADAPTER"}</span>
                            </div>
                          ))}
                        </div>
                      </div>
                      <p className="toolbox-note">密码只在点击连接时进入一次原生层；成功后前端立即清空它，浏览、下载和上传只发送随机连接 ID。下载与上传继续使用原生选择器、流式临时文件和不覆盖发布。断开只清理内存会话；“忘记”才会删除系统凭据。iHub 不做后台同步、自动上传或云端全文索引。</p>
                    </>
                  )}
                </section>
              ) : null}

              {activeTab === "record" ? (
                <section aria-labelledby="toolbox-record-title" id="toolbox-panel-record" role="tabpanel">
                  <div className="toolbox-section-heading">
                    <span className="toolbox-section-heading__icon"><Video size={17} /></span>
                    <div>
                      <h3 id="toolbox-record-title">屏幕录制</h3>
                      <p>使用系统屏幕选择器录制当前显示器、窗口或标签页。</p>
                    </div>
                  </div>
                  <div className={"recording-status" + (recordingPhase === "recording" ? " is-recording" : isRecordingPaused ? " is-paused" : isStoppingRecording ? " is-stopping" : "")}>
                    <span className="recording-status__dot" />
                    <div>
                      <small>
                        {recordingPhase === "recording"
                          ? "RECORDING · ACTIVE LIMIT"
                          : isRecordingPaused
                            ? "PAUSED · TIME FROZEN"
                            : isStoppingRecording
                              ? "SAVING WEBM"
                              : isStartingRecording
                                ? "WAITING FOR PERMISSION"
                                : "READY"}
                      </small>
                      <strong>
                        {recordingPhase === "recording"
                          ? `正在录制 ${formatElapsed(recordingElapsed)}`
                          : isRecordingPaused
                            ? `录制已暂停 ${formatElapsed(recordingElapsed)}`
                            : isStoppingRecording
                              ? "正在保存已录制的 WebM"
                              : isStartingRecording
                                ? "请选择要录制的屏幕或窗口"
                                : "准备开始屏幕录制"}
                      </strong>
                      {isRecording || isStoppingRecording ? (
                        <span className="recording-status__meta">
                          已缓存 {formatByteSize(recordingBytes)} / {formatByteSize(maxScreenRecordingBytes)} · 剩余活跃录制 {formatElapsed(Math.max(0, maxScreenRecordingDurationMs - recordingElapsed))}
                        </span>
                      ) : null}
                    </div>
                  </div>
                  <label className="toolbox-checkbox">
                    <input
                      checked={includeSystemAudio}
                      disabled={isRecordingSessionActive}
                      onChange={(event) => setIncludeSystemAudio(event.target.checked)}
                      type="checkbox"
                    />
                    请求录制系统音频（取决于系统与选择目标）
                  </label>
                  <div className="toolbox-action-row toolbox-record-actions">
                    <button
                      className={"toolbox-record-action" + (isRecordingSessionActive ? " is-recording" : "")}
                      disabled={isStartingRecording || isStoppingRecording}
                      onClick={() => void (isRecording ? stopScreenRecording("manual") : startScreenRecording())}
                      type="button"
                    >
                      {isRecordingSessionActive ? <span className="toolbox-record-action__stop" /> : <Video size={16} />}
                      {isStoppingRecording
                        ? "正在保存 WebM…"
                        : isRecording
                          ? "停止并保存 WebM"
                          : isStartingRecording
                            ? "正在打开系统选择器…"
                            : "选择屏幕并开始录制"}
                    </button>
                    {isRecording ? (
                      <button
                        className="toolbox-secondary-action"
                        onClick={isRecordingPaused ? resumeScreenRecording : pauseScreenRecording}
                        type="button"
                      >
                        {isRecordingPaused ? <Play size={14} /> : <Pause size={14} />}
                        {isRecordingPaused ? "继续录制" : "暂停录制"}
                      </button>
                    ) : null}
                  </div>
                  <p className="toolbox-note">录制完成后会下载 WebM 文件。浏览器会在本机内存中收集片段，达到 30 分钟活跃录制或 512 MB 触发阈值时会自动保存已录制部分；暂停不消耗 30 分钟倒计时。关闭工具箱会立刻停止并保存，系统选择器晚到的授权不会在后台开始录制。稳定 MP4、FFmpeg 转码、系统级快捷键和更深系统集成仍需独立的原生插件实现。</p>
                </section>
              ) : null}

              {activeTab === "rename" ? (
                <section aria-labelledby="toolbox-rename-title" id="toolbox-panel-rename" role="tabpanel">
                  <div className="toolbox-section-heading">
                    <span className="toolbox-section-heading__icon"><Files size={17} /></span>
                    <div>
                      <h3 id="toolbox-rename-title">批量重命名</h3>
                      <p>先生成预览，再以确认过的 from → to 清单执行。</p>
                    </div>
                  </div>
                  <div className="toolbox-field">
                    <span id="rename-directory-label">文件夹（系统选择）</span>
                    <div className="toolbox-directory-input">
                      <input
                        aria-labelledby="rename-directory-label"
                        placeholder="请使用系统文件夹选择器"
                        readOnly
                        value={renameDirectory}
                      />
                      <button
                        aria-label="选择批量重命名文件夹"
                        disabled={directoryPickerTarget !== null}
                        onClick={() => void chooseDirectory("rename")}
                        title="从系统选择文件夹"
                        type="button"
                      >
                        {directoryPickerTarget === "rename" ? <LoaderCircle className="spin" size={14} /> : <FolderSearch size={14} />}
                        选择
                      </button>
                    </div>
                  </div>
                  <div className="toolbox-field-grid">
                    <label className="toolbox-field">
                      <span>查找</span>
                      <input
                        onChange={(event) => { setRenameFind(event.target.value); clearRenamePreview(); }}
                        placeholder="IMG_"
                        value={renameFind}
                      />
                    </label>
                    <label className="toolbox-field">
                      <span>替换为</span>
                      <input
                        onChange={(event) => { setRenameReplace(event.target.value); clearRenamePreview(); }}
                        placeholder="trip-{n}-"
                        value={renameReplace}
                      />
                    </label>
                  </div>
                  <div className="toolbox-field-grid">
                    <label className="toolbox-field">
                      <span>序号起始</span>
                      <input
                        disabled={!renameReplace.includes("{n}")}
                        inputMode="numeric"
                        min="1"
                        onChange={(event) => { setRenameSequenceStart(event.target.value); clearRenamePreview(); }}
                        type="number"
                        value={renameSequenceStart}
                      />
                    </label>
                    <label className="toolbox-field">
                      <span>补零位数</span>
                      <input
                        disabled={!renameReplace.includes("{n}")}
                        inputMode="numeric"
                        max="12"
                        min="0"
                        onChange={(event) => { setRenameSequencePadding(event.target.value); clearRenamePreview(); }}
                        type="number"
                        value={renameSequencePadding}
                      />
                    </label>
                  </div>
                  <p className="toolbox-note">在“替换为”中加入 <code>{"{n}"}</code> 可按文件名顺序编号；默认从 001 开始，预览固定后才会执行。</p>
                  <label className="toolbox-checkbox">
                    <input
                      checked={renameUseRegex}
                      onChange={(event) => { setRenameUseRegex(event.target.checked); clearRenamePreview(); }}
                      type="checkbox"
                    />
                    将“查找”按正则表达式处理
                  </label>
                  <div className="toolbox-action-row">
                    <button
                      className="accent-button toolbox-primary-action"
                      disabled={isPreviewingRename || !renameDirectoryOpenId}
                      onClick={() => void previewRename()}
                      type="button"
                    >
                      {isPreviewingRename ? <LoaderCircle className="spin" size={15} /> : <Search size={15} />}
                      生成预览
                    </button>
                    {renamePreview?.canApply && renamePreview.items.length ? (
                      <button
                        className="toolbox-danger-action"
                        disabled={isApplyingRename}
                        onClick={() => void applyRename()}
                        type="button"
                      >
                        {isApplyingRename ? <LoaderCircle className="spin" size={15} /> : <ArrowRight size={15} />}
                        确认改名 {renamePreview.items.length} 项
                      </button>
                    ) : null}
                  </div>
                  {renamePreview ? (
                    <div className="rename-preview">
                      {renamePreview.errors.map((error) => (
                        <p className="toolbox-feedback is-error" key={error}>
                          <CircleAlert size={14} />
                          {displayLocalPath(error)}
                        </p>
                      ))}
                      {renamePreview.items.slice(0, 10).map((item) => (
                        <div className="rename-preview__item" key={`${item.from}-${item.to}`}>
                          <span title={displayLocalPath(item.from)}>{displayLocalPath(item.from)}</span>
                          <ArrowRight size={13} />
                          <strong title={displayLocalPath(item.to)}>{displayLocalPath(item.to)}</strong>
                        </div>
                      ))}
                      {renamePreview.items.length > 10 ? (
                        <p className="toolbox-note">另有 {renamePreview.items.length - 10} 个项目将在确认后执行。</p>
                      ) : null}
                      {!renamePreview.items.length && !renamePreview.errors.length ? (
                        <p className="toolbox-note">没有可改名的直接子文件。</p>
                      ) : null}
                    </div>
                  ) : null}
                </section>
              ) : null}

              {activeTab === "developer" ? (
                <section aria-labelledby="toolbox-developer-title" id="toolbox-panel-developer" role="tabpanel">
                  <div className="toolbox-section-heading">
                    <span className="toolbox-section-heading__icon"><Code2 size={17} /></span>
                    <div>
                      <h3 id="toolbox-developer-title">创建插件项目</h3>
                      <p>生成可立即链接的 TypeScript + Vite 前端模板；附带可选 Rust JSONL worker 样例，不覆盖已有目录。</p>
                    </div>
                  </div>
                  <div className="toolbox-field">
                    <span id="project-parent-directory-label">项目父目录（系统选择）</span>
                    <div className="toolbox-directory-input">
                      <input
                        aria-labelledby="project-parent-directory-label"
                        placeholder="请使用系统文件夹选择器"
                        readOnly
                        value={projectParentDirectory}
                      />
                      <button
                        aria-label="选择插件项目父目录"
                        disabled={directoryPickerTarget !== null}
                        onClick={() => void chooseDirectory("project")}
                        title="从系统选择目录"
                        type="button"
                      >
                        {directoryPickerTarget === "project" ? <LoaderCircle className="spin" size={14} /> : <FolderSearch size={14} />}
                        选择
                      </button>
                    </div>
                  </div>
                  <label className="toolbox-field">
                    <span>插件 ID（小写 kebab-case）</span>
                    <input
                      onChange={(event) => { setProjectId(event.target.value); setProjectResult(null); }}
                      placeholder="ihub-plugin-my-feature"
                      spellCheck="false"
                      value={projectId}
                    />
                  </label>
                  <div className="project-template-files" aria-label="将生成的文件">
                    <span>plugin.json</span>
                    <span>src/main.ts</span>
                    <span>vite.config.ts</span>
                    <span>worker/src/main.rs（可选）</span>
                    <span>scripts/build-worker.*（可选）</span>
                    <span>scripts/verify-plugin.mjs</span>
                    <span>docs/JSONL_RPC.md</span>
                    <span>docs/ENABLE_NATIVE_WORKER.md</span>
                    <span>README.md</span>
                  </div>
                  <button
                    className="accent-button toolbox-primary-action"
                    disabled={isCreatingProject || !projectParentDirectoryOpenId}
                    onClick={() => void createPluginProject()}
                    type="button"
                  >
                    {isCreatingProject ? <LoaderCircle className="spin" size={15} /> : <Code2 size={15} />}
                    创建 TypeScript 插件模板（含可选 Rust worker）
                  </button>
                  {projectResult ? (
                    <div className="project-result">
                      <p className="toolbox-feedback is-success">
                        <Check size={14} />
                        已创建 {projectResult.pluginId}
                      </p>
                      <code>{projectResult.projectPath}</code>
                      <ol>
                        {projectResult.nextSteps.map((step) => <li key={step}>{step}</li>)}
                      </ol>
                      <button
                        className="toolbox-secondary-action"
                        disabled={isOpeningProject}
                        onClick={() => void openCreatedProject()}
                        type="button"
                      >
                        {isOpeningProject ? <LoaderCircle className="spin" size={14} /> : <FolderSearch size={14} />}
                        打开项目文件夹
                      </button>
                      <p className="toolbox-note">下方已自动填入这个目录；先在项目中执行 <code>pnpm build</code>（包含静态预检）后再链接。发布到 GitHub 时还需提交 <code>plugin.json</code>、<code>dist/</code> 与已声明的 <code>bin/</code> 工件。</p>
                    </div>
                  ) : null}
                  <div className="local-plugin-linker">
                    <div className="local-plugin-linker__heading">
                      <strong>链接本地项目</strong>
                      <span>DEVELOPMENT LINK</span>
                    </div>
                    <p>不复制项目、不安装依赖、不执行脚本或二进制。iHub 只记录这个路径，并在重新打开插件前端时读取最新构建文件。</p>
                    <div className="toolbox-field">
                      <span id="local-plugin-directory-label">本地插件目录（系统选择）</span>
                      <div className="toolbox-directory-input">
                        <input
                          aria-labelledby="local-plugin-directory-label"
                          placeholder="请使用系统文件夹选择器"
                          readOnly
                          value={localPluginDirectory}
                        />
                        <button
                          aria-label="选择本地插件目录"
                          disabled={directoryPickerTarget !== null}
                          onClick={() => void chooseDirectory("local-plugin")}
                          title="从系统选择目录"
                          type="button"
                        >
                          {directoryPickerTarget === "local-plugin" ? <LoaderCircle className="spin" size={14} /> : <FolderSearch size={14} />}
                          选择
                        </button>
                      </div>
                    </div>
                    <p className="toolbox-note">推荐顺序：先在项目目录自行运行 <code>pnpm install</code>，再运行 <code>pnpm build</code>（包含静态预检）；确认 <code>dist/index.html</code> 已生成后再链接。链接不会代替或触发这些命令。</p>
                    <button
                      className="toolbox-secondary-action local-plugin-linker__action"
                      disabled={isLinkingLocalPlugin || !localPluginDirectoryOpenId}
                      onClick={() => void linkLocalPlugin()}
                      type="button"
                    >
                      {isLinkingLocalPlugin ? <LoaderCircle className="spin" size={15} /> : <FolderSearch size={15} />}
                      链接本地插件
                    </button>
                    {developmentPlugins.length ? (
                      <div className="local-plugin-links" aria-label="已链接的本地插件">
                        {developmentPlugins.map((plugin) => (
                          <div className="local-plugin-link" key={plugin.id}>
                            <div>
                              <strong>{plugin.name}</strong>
                              <code>{displayLocalPath(plugin.localPath ?? plugin.source ?? "")}</code>
                            </div>
                            <button
                              disabled={unlinkingPluginId === plugin.id}
                              onClick={() => void unlinkLocalPlugin(plugin)}
                              type="button"
                            >
                              {unlinkingPluginId === plugin.id ? <LoaderCircle className="spin" size={13} /> : <X size={13} />}
                              解除链接
                            </button>
                          </div>
                        ))}
                      </div>
                    ) : null}
                  </div>
                  <p className="toolbox-note">模板不会自动执行依赖安装或脚本；先审阅内容，再在项目目录运行提示的命令。</p>
                </section>
              ) : null}
            </div>
              </>
            )}
          </motion.aside>
        </>
      ) : null}
    </AnimatePresence>
  );
}
