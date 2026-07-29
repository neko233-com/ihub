import { check, type Update } from "@tauri-apps/plugin-updater";
import { LogicalSize } from "@tauri-apps/api/dpi";
import { getCurrentWindow } from "@tauri-apps/api/window";
import {
  Braces,
  Calculator,
  Camera,
  Check,
  CircleAlert,
  Clock3,
  Download,
  FileText,
  Files,
  Folder,
  FolderSearch,
  History,
  Keyboard,
  LoaderCircle,
  LogOut,
  Power,
  Puzzle,
  RefreshCw,
  Settings2,
  Sparkles,
  X,
  Zap,
} from "lucide-react";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import {
  lazy,
  Suspense,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  builtinPinnedItems,
  defaultMarketplaceItems,
  SpotlightLauncher,
  type LauncherPastedImage,
  type SpotlightLauncherItem,
} from "./components/SpotlightLauncher";
import type { RecordingPhase, ToolboxLaunchContext, ToolboxTab } from "./components/ToolboxDrawer";
import {
  availableLauncherContextActions,
  deriveLauncherContextActions,
  type LauncherContextAction,
} from "./lib/launcher-context-actions";
import {
  eligibleLauncherContextCommands,
  previewLauncherContextHandoff,
  type LauncherContextHandoff,
  type LauncherContextImageSource,
} from "./lib/plugin-launcher-context";
import { command, isDesktop, onFocusSearch, onHideSearch } from "./lib/desktop";
import {
  findLauncherContentResults,
  quickNotesStorageKey,
  readLauncherQuickNotes,
} from "./lib/launcher-content-search";
import {
  formatLauncherHotkey,
  normalizeLauncherHotkey,
  type LauncherHotkeyRejectionReason,
} from "./lib/launcher-hotkey";
import {
  describeLauncherHotkey,
  launcherHotkeyResetAction,
} from "./lib/launcher-hotkey-status";
import {
  buildLauncherItemIndex,
  isLauncherRecentDestination,
  LAUNCHER_RECENT_CAPACITY,
  retainLauncherRecent,
} from "./lib/launcher-home";
import {
  mergeNativeIconCache,
  nativeIconForLauncherShortcut,
  nativeIconForResult,
  safeNativeIconSrc,
  sanitizeSystemIconMap,
  systemIconRequestChunks,
  type SystemIconMap,
} from "./lib/native-icons";
import { launcherCalculationResults } from "./lib/launcher-calculation";
import { mergeLauncherSearchResults } from "./lib/launcher-ranking";
import { launcherSystemCommandResults } from "./lib/launcher-system-commands";
import { mockResults } from "./lib/mock-data";
import {
  parseLauncherTimeInput,
  shouldOfferLauncherTimeTool,
} from "./lib/time-tools";
import {
  canInstallDiscoveredUpdate,
  recordAutomaticUpdateAttempt,
  type UpdateInstallOrigin,
} from "./lib/update-install-policy";
import type {
  AppHealth,
  AutostartStatus,
  ClipboardFile,
  ClipboardImage,
  ClipboardHistorySnapshot,
  IndexStatus,
  LauncherHotkeyStatus,
  LauncherShortcutView,
  PluginCommandInfo,
  PluginCommandResult,
  PluginFrontendEvent,
  PluginInfo,
  PluginLauncherContextIssue,
  PluginSearchProviderInfo,
  PluginSearchResponse,
  SearchResult,
} from "./lib/types";

const browserStatus: IndexStatus = {
  phase: "ready",
  indexedFiles: 0,
  roots: [],
  message: "浏览器预览模式",
};

type UpdatePhase =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "installing"
  | "installed"
  | "error";

interface UpdateProgress {
  received: number;
  total?: number;
}

type LauncherSurface =
  | "hidden"
  | "launcher"
  | "plugin-center"
  | "toolbox"
  | "settings"
  | "plugin";

const isDevelopmentBuild = import.meta.env.DEV;
const UPDATE_DISCOVERY_INTERVAL_MS = 6 * 60 * 60 * 1000;
const launcherRecentStorageKey = "ihub.launcher.recent-command-ids.v1";
const launcherRecentApplicationsStorageKey = "ihub.launcher.recent-applications.v1";
const launcherShowRecentStorageKey = "ihub.launcher.show-recent.v1";
// Installation remains an explicit opt-in. Keeping this separate from update
// discovery lets every production client learn about a signed release without
// unexpectedly handing its process to a platform installer.
const autoInstallSignedUpdatesStorageKey = "ihub.updater.auto-install-signed-releases.v1";
const autoInstallAttemptedVersionsStorageKey = "ihub.updater.auto-install-attempted-versions.v1";

function nativePluginCommandSummary(result: PluginCommandResult): string | null {
  const output = result.output;
  const message = output && typeof output === "object" && "message" in output
    ? (output as { message?: unknown }).message
    : undefined;
  const preferred = typeof message === "string"
    ? message
    : typeof output === "string"
      ? output
      : result.stdout.trim();
  const normalized = preferred.replace(/\s+/g, " ").trim();
  return normalized ? normalized.slice(0, 180) : null;
}
const launcherSpaceActivatesStorageKey = "ihub.launcher.space-activates.v1";
const launcherPinnedStorageKey = "ihub.launcher.pinned-item-ids.v1";
const launcherShortcutItemPrefix = "ihub.launcher-shortcut:";

function launcherHotkeyRejectionMessage(reason: LauncherHotkeyRejectionReason): string {
  switch (reason) {
    case "modifier-required":
      return "请同时按住 Ctrl / Command 或 Alt / Option。";
    case "reserved-key":
      return "这个按键会影响系统或输入操作，请换一个组合。";
    case "reserved-shortcut":
      return "Alt / Option + F4 是系统关闭快捷键，不能使用。";
    case "unsupported-key":
      return "暂不支持这个按键；可使用字母、数字、F1–F12、Space 或常用标点。";
    case "modifier-only":
      return "继续按住修饰键，再按一个普通按键。";
  }
}

// The launcher opens on every hotkey press, while the marketplace, utility
// suite, and third-party iframe host are occasional surfaces. Split them out
// so the Spotlight path stays responsive on a cold start.
const PluginCenter = lazy(async () => {
  const module = await import("./components/PluginCenter");
  return { default: module.PluginCenter };
});
const ToolboxDrawer = lazy(async () => {
  const module = await import("./components/ToolboxDrawer");
  return { default: module.ToolboxDrawer };
});
const PluginFrontendFrame = lazy(async () => {
  const module = await import("./components/PluginFrontendFrame");
  return { default: module.PluginFrontendFrame };
});

function readStoredStringArray(key: string, limit = 12): string[] {
  if (typeof window === "undefined") {
    return [];
  }
  try {
    const value: unknown = JSON.parse(window.localStorage.getItem(key) ?? "[]");
    return Array.isArray(value)
      ? value.filter((item): item is string => typeof item === "string").slice(0, limit)
      : [];
  } catch {
    return [];
  }
}

function readStoredPinnedItemIds(): string[] {
  const defaults = builtinPinnedItems.map((item) => item.id);
  if (typeof window === "undefined") {
    return defaults;
  }
  try {
    const raw = window.localStorage.getItem(launcherPinnedStorageKey);
    if (raw === null) {
      return defaults;
    }
    const value: unknown = JSON.parse(raw);
    if (!Array.isArray(value)) {
      return defaults;
    }
    return Array.from(new Set(
      value.filter((item): item is string => typeof item === "string" && item.trim().length > 0),
    )).slice(0, 30);
  } catch {
    return defaults;
  }
}

function readStoredBoolean(key: string, fallback: boolean) {
  if (typeof window === "undefined") {
    return fallback;
  }
  try {
    const value = window.localStorage.getItem(key);
    return value === null ? fallback : value !== "false";
  } catch {
    return fallback;
  }
}

/**
 * System applications are discovered only by the native host. Persisting this
 * small, validated snapshot lets a deliberately launched app remain in the
 * uTools-style recent row after the query is cleared, without inventing an
 * application catalog in browser preview.
 */
function readStoredRecentApplications(): SearchResult[] {
  if (typeof window === "undefined") {
    return [];
  }
  try {
    const value: unknown = JSON.parse(window.localStorage.getItem(launcherRecentApplicationsStorageKey) ?? "[]");
    if (!Array.isArray(value)) {
      return [];
    }
    const recentApplications = value.flatMap((candidate) => {
      if (!candidate || typeof candidate !== "object") {
        return [];
      }
      const item = candidate as Partial<SearchResult>;
      if (
        item.kind !== "application"
        || typeof item.id !== "string"
        || typeof item.name !== "string"
        || typeof item.path !== "string"
        || !item.id
        || !item.name
        || !item.path
      ) {
        return [];
      }
      return [{
        id: item.id,
        name: item.name,
        path: item.path,
        kind: "application" as const,
        score: 0,
        metadata: typeof item.metadata === "string" ? item.metadata : undefined,
      }];
    });
    return retainLauncherRecent(recentApplications);
  } catch {
    return [];
  }
}

function persistLauncherValue(key: string, value: unknown) {
  try {
    window.localStorage.setItem(key, JSON.stringify(value));
  } catch {
    // History is an enhancement; private/locked storage must not break launch.
  }
}

async function requestSystemIconMap(
  searchResultIds: readonly string[],
  launcherShortcutIds: readonly string[],
): Promise<SystemIconMap> {
  const allowedIds = new Set([...searchResultIds, ...launcherShortcutIds]);
  const merged: SystemIconMap = {};
  for (const request of systemIconRequestChunks(searchResultIds, launcherShortcutIds)) {
    const response = await command<unknown>("get_system_icons", {
      searchResultIds: request.searchResultIds,
      launcherShortcutIds: request.launcherShortcutIds,
    });
    Object.assign(merged, sanitizeSystemIconMap(response, allowedIds));
  }
  return merged;
}

function filterPreviewResults(query: string) {
  const normalized = query.trim().toLocaleLowerCase();
  if (!normalized) {
    return mockResults;
  }

  return mockResults.filter((item) =>
    [item.name, item.metadata, item.path]
      .filter(Boolean)
      .join(" ")
      .toLocaleLowerCase()
      .includes(normalized),
  );
}

function spotlightItemForSearchResult(
  result: SearchResult,
  nativeIconSrc?: string,
): SpotlightLauncherItem {
  const normalizedNativeIconSrc = safeNativeIconSrc(nativeIconSrc);
  const isCalculatorResult = typeof result.calculatorExpression === "string";
  const isTimeResult = result.commandId === "ihub.tool.time";
  const isSettingsResult = result.commandId === "ihub.open-settings";
  const icon = isCalculatorResult
    ? Calculator
    : isTimeResult
      ? Clock3
      : isSettingsResult
        ? Settings2
        : result.kind === "file"
          ? FileText
          : result.kind === "folder"
            ? Folder
            : result.kind === "application"
              ? undefined
              : result.kind === "plugin"
                ? Puzzle
                : Zap;
  const tone = isCalculatorResult
    ? "amber"
    : isTimeResult
      ? "blue"
      : isSettingsResult
        ? "slate"
        : result.kind === "file"
          ? "slate"
          : result.kind === "folder"
            ? "mint"
            : result.kind === "application"
              ? "blue"
              : result.kind === "plugin"
                ? "violet"
                : "amber";
  const badge = isCalculatorResult
    ? "计算"
    : isTimeResult
      ? "时间"
      : isSettingsResult
        ? "系统"
        : result.kind === "file"
          ? "文件"
          : result.kind === "folder"
            ? "文件夹"
            : result.kind === "application"
              ? "应用"
              : result.kind === "plugin"
                ? "插件"
                : "命令";

  return {
    id: result.id,
    label: result.name,
    detail: result.metadata ?? result.path ?? undefined,
    badge,
    icon,
    iconSrc: normalizedNativeIconSrc,
    nativeIconPending: result.kind === "application" && !normalizedNativeIconSrc,
    tone,
    canPinFromSearch: result.pinEligible === true,
    pinnedShortcutId: result.pinnedShortcutId,
  };
}

function spotlightItemForContextAction(action: LauncherContextAction): SpotlightLauncherItem {
  const iconAndTone = (() => {
    switch (action.id) {
      case "ihub.context.json":
        return { icon: Braces, tone: "amber" as const };
      case "ihub.context.local-search":
        return { icon: FolderSearch, tone: "mint" as const };
      case "ihub.context.batch-rename":
        return { icon: Files, tone: "slate" as const };
      case "ihub.context.screenshot":
        return { icon: Camera, tone: "blue" as const };
      case "ihub.context.translate":
        return { icon: Sparkles, tone: "violet" as const };
      default:
        return { icon: Puzzle, tone: "violet" as const };
    }
  })();

  return {
    id: action.id,
    label: action.label,
    detail: action.detail,
    badge: action.target.kind === "builtin" ? "内置" : "插件中心",
    ...iconAndTone,
  };
}

function launcherShortcutItemId(shortcutId: string) {
  return `${launcherShortcutItemPrefix}${shortcutId}`;
}

function shortcutIdFromLauncherItemId(itemId: string): string | null {
  const shortcutId = itemId.startsWith(launcherShortcutItemPrefix)
    ? itemId.slice(launcherShortcutItemPrefix.length)
    : "";
  return /^[0-9a-f]{8}-(?:[0-9a-f]{4}-){3}[0-9a-f]{12}$/i.test(shortcutId)
    ? shortcutId
    : null;
}

function spotlightItemForLauncherShortcut(
  shortcut: LauncherShortcutView,
  nativeIconSrc?: string,
): SpotlightLauncherItem {
  const unavailable = shortcut.status !== "ready";
  const item = spotlightItemForSearchResult({
    id: launcherShortcutItemId(shortcut.id),
    name: shortcut.name,
    kind: shortcut.kind,
    score: 0,
    metadata: !unavailable
      ? shortcut.metadata
      : [shortcut.metadata, "目标当前不可用"].filter(Boolean).join(" · "),
  }, nativeIconSrc);
  return {
    ...item,
    badge: unavailable ? "不可用" : item.badge,
    unavailable,
  };
}

function pluginCommandResults(plugins: PluginInfo[], query: string): SearchResult[] {
  const normalized = query.trim().toLocaleLowerCase();

  return plugins.flatMap((plugin, pluginIndex) => {
    if (plugin.enabled === false || !Array.isArray(plugin.commands)) {
      return [];
    }

    return plugin.commands
      .filter((command): command is PluginCommandInfo => Boolean(command?.id))
      .filter((command) => {
        if (!normalized) {
          return true;
        }
        return [plugin.name, plugin.description, command.id, command.name, command.description]
          .filter(Boolean)
          .join(" ")
          .toLocaleLowerCase()
          .includes(normalized);
      })
      .map((command, commandIndex) => ({
        id: `plugin-command:${plugin.id}:${command.id}`,
        name: command.name || command.id,
        kind: "plugin" as const,
        score: 900 - pluginIndex * 10 - commandIndex,
        metadata: [plugin.name, command.description].filter(Boolean).join(" · "),
        pluginId: plugin.id,
        commandId: command.id,
      }));
  });
}

const MAX_LAUNCHER_PLUGIN_PROVIDERS = 3;
const MAX_LAUNCHER_RESULTS_PER_PROVIDER = 3;

interface EligiblePluginSearchProvider {
  plugin: PluginInfo;
  provider: PluginSearchProviderInfo;
  query: string;
}

function pluginSearchProviderKey(pluginId: string, providerId: string) {
  return `${pluginId}:${providerId}`;
}

/**
 * Providers with a trigger run only after that prefix, while no-trigger
 * providers may participate in normal launcher text search. The final cap is
 * intentional: third-party frontends never fan out without a fixed bound.
 */
function eligiblePluginSearchProviders(
  plugins: PluginInfo[],
  query: string,
  registeredKeys?: ReadonlySet<string>,
): EligiblePluginSearchProvider[] {
  const normalizedQuery = query.trim();
  if (!normalizedQuery) {
    return [];
  }
  const normalizedLower = normalizedQuery.toLocaleLowerCase();

  return plugins
    .flatMap((plugin) => {
      if (plugin.enabled === false || !plugin.frontendEntry || !Array.isArray(plugin.searchProviders)) {
        return [];
      }
      return plugin.searchProviders.flatMap((provider) => {
        if (
          !provider?.id
          || (registeredKeys && !registeredKeys.has(pluginSearchProviderKey(plugin.id, provider.id)))
        ) {
          return [];
        }
        const trigger = provider.trigger;
        if (trigger?.trim()) {
          const normalizedTrigger = trigger.toLocaleLowerCase();
          if (!normalizedLower.startsWith(normalizedTrigger)) {
            return [];
          }
          const providerQuery = normalizedQuery.slice(trigger.length).trim();
          return providerQuery ? [{ plugin, provider, query: providerQuery }] : [];
        }
        return [{ plugin, provider, query: normalizedQuery }];
      });
    })
    .sort((left, right) =>
      (right.provider.priority ?? 0) - (left.provider.priority ?? 0)
      || left.plugin.name.localeCompare(right.plugin.name)
      || left.provider.title.localeCompare(right.provider.title),
    )
    .slice(0, MAX_LAUNCHER_PLUGIN_PROVIDERS);
}

function pluginProviderResponseResults(
  response: PluginSearchResponse,
  provider: EligiblePluginSearchProvider,
): SearchResult[] {
  return response.results.slice(0, MAX_LAUNCHER_RESULTS_PER_PROVIDER).map((result, index) => ({
    id: `plugin-search:${provider.plugin.id}:${provider.provider.id}:${encodeURIComponent(result.id)}`,
    name: result.title,
    kind: "plugin" as const,
    score: 840 + (provider.provider.priority ?? 0) + Math.max(-1_000, Math.min(1_000, result.score)) - index / 100,
    metadata: [
      "插件搜索",
      provider.plugin.name,
      provider.provider.title,
      result.subtitle,
    ].filter(Boolean).join(" · "),
    pluginId: provider.plugin.id,
    pluginProviderId: provider.provider.id,
    pluginSearchResultId: result.id,
    pluginPayload: result.payload,
  }));
}

const builtinTools: Array<{
  commandId: string;
  keywords?: string[];
  metadata: string;
  name: string;
  tab: ToolboxTab;
}> = [
  {
    commandId: "ihub.tool.local-search",
    name: "本地搜索与索引",
    metadata: "查看索引范围并立即刷新",
    tab: "search",
  },
  {
    commandId: "ihub.tool.color",
    name: "颜色工具",
    metadata: "HEX / RGB / HSL 转换与复制",
    tab: "color",
  },
  {
    commandId: "ihub.tool.screenshot",
    name: "截图",
    metadata: "选择屏幕、窗口或标签页并导出 PNG",
    tab: "screenshot",
  },
  {
    commandId: "ihub.tool.clipboard-history",
    name: "剪贴板历史",
    metadata: "本机保存、固定和复用文本记录",
    tab: "clipboard",
  },
  {
    commandId: "ihub.tool.json",
    name: "JSON 格式化与校验",
    metadata: "离线处理 JSON 内容",
    tab: "json",
  },
  {
    commandId: "ihub.tool.markdown",
    name: "Markdown 工作台",
    metadata: "离线写作、安全预览、目录定位与本地导出",
    tab: "markdown",
  },
  {
    commandId: "ihub.tool.quick-note",
    name: "快速便签",
    metadata: "在本机保存、搜索和复制临时笔记",
    tab: "note",
  },
  {
    commandId: "ihub.tool.convert",
    name: "进制与文本转换",
    metadata: "BigInt 进制、UTF-8 Hex 与 Base64 转换",
    tab: "convert",
  },
  {
    commandId: "ihub.tool.calculator",
    name: "计算器",
    metadata: "离线计算四则、括号、百分号、幂与小数表达式",
    tab: "calculator",
  },
  {
    commandId: "ihub.tool.time",
    name: "时间与时间戳",
    metadata: "Unix 秒/毫秒、日期、ISO、UTC 与 IANA 时区转换",
    keywords: ["时间戳", "timestamp", "unix", "epoch", "日期", "时区", "timezone", "10位", "13位"],
    tab: "time",
  },
  {
    commandId: "ihub.tool.qrcode",
    name: "二维码",
    metadata: "离线生成二维码、导出 PNG，并识别你选择的本地图片",
    tab: "qrcode",
  },
  {
    commandId: "ihub.tool.cloud-drive",
    name: "云盘（WebDAV）",
    metadata: "受限原生连接器；显式连接后浏览 WebDAV 目录，不保存账号密码",
    tab: "cloud",
  },
  {
    commandId: "ihub.tool.screen-record",
    name: "屏幕录制",
    metadata: "选择屏幕、窗口或标签页并导出 WebM",
    tab: "record",
  },
  {
    commandId: "ihub.tool.batch-rename",
    name: "批量重命名",
    metadata: "预览后再安全执行文件改名",
    tab: "rename",
  },
  {
    commandId: "ihub.tool.create-plugin",
    name: "创建 iHub 插件项目",
    metadata: "生成 TypeScript 前端 + Rust worker 开发模板",
    tab: "developer",
  },
];

function builtinToolResults(query: string): SearchResult[] {
  const normalized = query.trim().toLocaleLowerCase();
  const parsedTimeQuery = normalized ? parseLauncherTimeInput(query) : null;
  return builtinTools
    .filter((tool) => {
      const searchableText = [tool.name, tool.metadata, tool.commandId, ...(tool.keywords ?? [])]
        .join(" ");
      if (tool.tab === "time") {
        return shouldOfferLauncherTimeTool(query, searchableText);
      }
      if (!normalized) {
        return true;
      }
      return searchableText
        .toLocaleLowerCase()
        .includes(normalized);
    })
    .map((tool, index) => ({
      id: `builtin-command:${tool.commandId}`,
      name: tool.name,
      kind: "command" as const,
      score: 980 - index,
      metadata: `内置工具 · ${tool.metadata}`,
      commandId: tool.commandId,
      timeInput: tool.tab === "time" && parsedTimeQuery?.ok ? query.trim() : undefined,
    }));
}

function toolboxTabForCommand(commandId?: string): ToolboxTab | null {
  return builtinTools.find((tool) => tool.commandId === commandId)?.tab ?? null;
}

function createFrontendCommandEvent(pluginId: string, commandId: string): PluginFrontendEvent {
  const suffix =
    typeof crypto !== "undefined" && "randomUUID" in crypto
      ? crypto.randomUUID()
      : Math.random().toString(36).slice(2);
  const requestId = `launcher-${Date.now().toString(36)}-${suffix}`;

  return {
    id: requestId,
    pluginId,
    name: `ihub://plugin/${pluginId}/command`,
    payload: {
      requestId,
      commandId,
      input: null,
      context: null,
    },
  };
}

function createFrontendSearchSelectionEvent(
  pluginId: string,
  providerId: string,
  resultId: string,
  payload: unknown,
): PluginFrontendEvent {
  const suffix =
    typeof crypto !== "undefined" && "randomUUID" in crypto
      ? crypto.randomUUID()
      : Math.random().toString(36).slice(2);
  const requestId = `launcher-search-${Date.now().toString(36)}-${suffix}`;

  return {
    id: requestId,
    pluginId,
    name: `ihub://plugin/${pluginId}/event/search.select`,
    payload: {
      requestId,
      providerId,
      resultId,
      payload: payload ?? null,
    },
  };
}

interface PendingLauncherContextDispatch {
  handoff: LauncherContextHandoff;
  plugin: PluginInfo;
  command: PluginCommandInfo;
  /** Every launch-panel transition invalidates the previous generation before
   * native work can continue. This guards the async gap between iframe-ready,
   * metadata construction, token issue, and event emission. */
  generation: number;
  /** The exact frontend source that was visible when the person confirmed.
   * A plugin refresh can keep its id while replacing its code/lease. */
  pluginSourceKey: string;
}

interface ActiveLauncherContextSurface {
  pluginId: string;
  leaseId: string;
  generation: number;
  pluginSourceKey: string;
}

interface IssuedLauncherContext {
  pluginId: string;
  contextId: string;
  /** Bind the renderer-side revocation handle to the exact confirmation and
   * iframe that created it. A stale async continuation must never replace or
   * clear a newer handoff's handle. */
  generation: number;
  leaseId: string;
  pluginSourceKey: string;
}

function sameIssuedLauncherContext(
  left: IssuedLauncherContext | null | undefined,
  right: IssuedLauncherContext | null | undefined,
) {
  return Boolean(
    left
    && right
    && left.pluginId === right.pluginId
    && left.contextId === right.contextId
    && left.generation === right.generation
    && left.leaseId === right.leaseId
    && left.pluginSourceKey === right.pluginSourceKey,
  );
}

interface HostLauncherContextRequest {
  text: string | null;
  files: Array<{ path: string }>;
  image: {
    name: string;
    mimeType: "image/png";
    width: number;
    height: number;
  } | null;
}

function nextLauncherContextUiId() {
  const suffix =
    typeof crypto !== "undefined" && "randomUUID" in crypto
      ? crypto.randomUUID()
      : Math.random().toString(36).slice(2);
  return `launcher-context-ui-${Date.now().toString(36)}-${suffix}`;
}

/** Keep the parent-side source identity in lockstep with PluginFrontendFrame.
 * The host lease is authoritative, but checking this before every async
 * continuation also prevents stale renderer work from reaching the host. */
function pluginFrontendSourceKey(plugin: PluginInfo | null): string | null {
  if (!plugin) {
    return null;
  }
  return [
    plugin.id,
    plugin.enabled === false ? "disabled" : "enabled",
    plugin.frontendEntry ?? "",
    plugin.commit ?? "",
    plugin.installedAt ?? "",
    plugin.sourceLock?.resolvedCommit ?? "",
    plugin.sourceLock?.installedAt ?? "",
    plugin.isDevelopmentLink ? "local" : "managed",
    plugin.localPath ?? "",
  ].join("\u0000");
}

/**
 * Dimension decoding happens only after the person has picked a concrete
 * plugin command and confirmed the handoff. The resulting pixels never leave
 * the renderer; Rust receives only a bounded PNG metadata record.
 */
async function readPastedImageDimensions(image: LauncherContextImageSource) {
  if (typeof createImageBitmap === "function") {
    const bitmap = await createImageBitmap(image.blob);
    try {
      return { width: bitmap.width, height: bitmap.height };
    } finally {
      bitmap.close();
    }
  }

  return new Promise<{ width: number; height: number }>((resolve, reject) => {
    const preview = new Image();
    const source = URL.createObjectURL(image.blob);
    preview.onload = () => {
      URL.revokeObjectURL(source);
      resolve({ width: preview.naturalWidth, height: preview.naturalHeight });
    };
    preview.onerror = () => {
      URL.revokeObjectURL(source);
      reject(new Error("无法读取已粘贴图片的尺寸。"));
    };
    preview.src = source;
  });
}

async function hostRequestForLauncherContext(
  handoff: LauncherContextHandoff,
): Promise<HostLauncherContextRequest> {
  switch (handoff.kind) {
    case "text":
      return { text: handoff.text, files: [], image: null };
    case "files":
      return {
        text: null,
        files: handoff.files.map((file) => ({ path: file.path })),
        image: null,
      };
    case "image": {
      const mimeType = handoff.image.type.toLocaleLowerCase().split(";", 1)[0] || "image/png";
      if (mimeType !== "image/png") {
        throw new Error("图片上下文目前仅支持 PNG；请粘贴 PNG 图片后重试。图片像素不会交给插件。");
      }
      const dimensions = await readPastedImageDimensions(handoff.image);
      return {
        text: null,
        files: [],
        image: {
          name: handoff.image.name || "ihub-pasted-image.png",
          mimeType: "image/png",
          ...dimensions,
        },
      };
    }
  }
}

export function App() {
  const booted = useRef(false);
  const updateRef = useRef<Update | null>(null);
  const updateCheckInFlightRef = useRef<Promise<void> | null>(null);
  const updateDiscoveryActiveRef = useRef(false);
  const updateInstallInFlightRef = useRef(false);
  const updateInstalledRef = useRef(false);
  // Keep automatic attempt versions across restarts too. A person can always
  // use the retained manual action, but a malformed release must not cause an
  // automatic install loop every time iHub launches.
  const autoInstallAttemptedVersionsRef = useRef(
    new Set(readStoredStringArray(autoInstallAttemptedVersionsStorageKey)),
  );
  const approvedNativePlugins = useRef(new Set<string>());
  const searchRequestRef = useRef(0);
  const homeIconRequestRef = useRef(0);
  const toolboxContextRequestRef = useRef(0);
  const launcherContextDispatchKeyRef = useRef<string | null>(null);
  const launcherContextGenerationRef = useRef(0);
  const pendingLauncherContextDispatchRef = useRef<PendingLauncherContextDispatch | null>(null);
  const activeLauncherContextSurfaceRef = useRef<ActiveLauncherContextSurface | null>(null);
  const issuedLauncherContextRef = useRef<IssuedLauncherContext | null>(null);
  const issuedLauncherContextExpiryTimerRef = useRef<number | null>(null);
  const pluginsRef = useRef<PluginInfo[]>([]);
  // A drag belongs only to the current visible launcher session. Native reveal
  // resets the window to center; panel resizes must preserve a deliberate drag
  // until that next reveal instead of snapping it back underneath the pointer.
  const windowWasDraggedThisRevealRef = useRef(false);
  const prefersReducedMotion = useReducedMotion();
  const [query, setQuery] = useState("");
  const [pastedFileResults, setPastedFileResults] = useState<SearchResult[]>([]);
  const [pastedImage, setPastedImage] = useState<LauncherPastedImage | null>(null);
  const [searchResults, setSearchResults] = useState<SearchResult[]>(mockResults);
  const [searchIconCache, setSearchIconCache] = useState<SystemIconMap>({});
  const [pluginSearchResults, setPluginSearchResults] = useState<SearchResult[]>([]);
  const [registeredSearchProviderKeys, setRegisteredSearchProviderKeys] = useState<string[]>([]);
  const [requestedSearchRuntimePluginIds, setRequestedSearchRuntimePluginIds] = useState<string[]>([]);
  const [quickNotes, setQuickNotes] = useState(readLauncherQuickNotes);
  // Browser preview keeps this null forever: clipboard records only come from
  // the desktop command after the user explicitly enables history.
  const [clipboardHistory, setClipboardHistory] = useState<ClipboardHistorySnapshot | null>(null);
  const [status, setStatus] = useState<IndexStatus>(browserStatus);
  // Browser preview intentionally starts with no third-party plugins. Desktop
  // startup replaces this with the real local registry, so catalog bootstrap
  // entries never masquerade as installed packages.
  const [plugins, setPlugins] = useState<PluginInfo[]>([]);
  const [health, setHealth] = useState<AppHealth | null>(null);
  const [surface, setSurface] = useState<LauncherSurface>("launcher");
  const [toolboxTab, setToolboxTab] = useState<ToolboxTab>("search");
  const [toolboxRecordingPhase, setToolboxRecordingPhase] = useState<RecordingPhase>("idle");
  const [toolboxLaunchContext, setToolboxLaunchContext] = useState<ToolboxLaunchContext | null>(null);
  const [pluginCenterInitialSearch, setPluginCenterInitialSearch] = useState<string | null>(null);
  // These values are renderer-session-only. A Plugin Center suggestion never
  // stages a host token; a source survives here only until one command is
  // visibly confirmed or the panel/iframe is cancelled.
  const [pluginCenterLauncherContext, setPluginCenterLauncherContext] = useState<LauncherContextHandoff | null>(null);
  const [pendingLauncherContextDispatch, setPendingLauncherContextDispatch] = useState<PendingLauncherContextDispatch | null>(null);
  const [launcherFocusSignal, setLauncherFocusSignal] = useState(0);
  const [pinnedItemIds, setPinnedItemIds] = useState(readStoredPinnedItemIds);
  // Native storage owns target paths and source IDs. The renderer sees only
  // opaque display views and can never turn localStorage into an open-path
  // authorization channel.
  const [launcherShortcuts, setLauncherShortcuts] = useState<LauncherShortcutView[]>([]);
  const [recentItemIds, setRecentItemIds] = useState<string[]>(() =>
    readStoredStringArray(launcherRecentStorageKey, LAUNCHER_RECENT_CAPACITY)
      .filter(isLauncherRecentDestination));
  const [recentApplications, setRecentApplications] = useState<SearchResult[]>(readStoredRecentApplications);
  const [homeIconCache, setHomeIconCache] = useState<SystemIconMap>({});
  const [showRecent, setShowRecent] = useState(() => readStoredBoolean(launcherShowRecentStorageKey, true));
  const [spaceActivates, setSpaceActivates] = useState(() => readStoredBoolean(launcherSpaceActivatesStorageKey, true));
  const [autoInstallSignedUpdates, setAutoInstallSignedUpdates] = useState(() =>
    readStoredBoolean(autoInstallSignedUpdatesStorageKey, false),
  );
  // The updater check resolves outside React's event cycle. Keep the latest
  // preference in a ref so turning the switch off while a check is underway
  // reliably prevents the completion handler from starting an install.
  const autoInstallSignedUpdatesRef = useRef(autoInstallSignedUpdates);
  const [activePlugin, setActivePlugin] = useState<PluginInfo | null>(null);
  const [pendingPluginEvent, setPendingPluginEvent] = useState<PluginFrontendEvent | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [availableUpdate, setAvailableUpdate] = useState<Update | null>(null);
  const [updatePhase, setUpdatePhase] = useState<UpdatePhase>("idle");
  const [updateProgress, setUpdateProgress] = useState<UpdateProgress>({ received: 0 });
  const [updateError, setUpdateError] = useState<string | null>(null);
  const [autostartEnabled, setAutostartEnabled] = useState<boolean | null>(null);
  const [isUpdatingAutostart, setIsUpdatingAutostart] = useState(false);
  const [autostartError, setAutostartError] = useState<string | null>(null);
  const [isRecordingLauncherHotkey, setIsRecordingLauncherHotkey] = useState(false);
  const [launcherHotkeyDraft, setLauncherHotkeyDraft] = useState<string | null>(null);
  const [isUpdatingLauncherHotkey, setIsUpdatingLauncherHotkey] = useState(false);
  const [launcherHotkeyError, setLauncherHotkeyError] = useState<string | null>(null);

  // Keep an imperative snapshot for lifecycle callbacks and async handoff
  // continuations. React state remains the UI source of truth; this ref only
  // lets cancellation take effect synchronously before a state render lands.
  pluginsRef.current = plugins;

  const revokeIssuedLauncherContext = useCallback((issued: IssuedLauncherContext | null) => {
    if (!issued || !isDesktop()) {
      return;
    }
    void command<boolean>("revoke_plugin_launcher_context", {
      pluginId: issued.pluginId,
      contextId: issued.contextId,
    }).catch(() => false);
  }, []);

  /** Clear only the exact retained token. Old async chains use this identity
   * comparison so they cannot erase a newer confirmation's revocation handle. */
  const clearIssuedLauncherContextReference = useCallback((issued: IssuedLauncherContext | null) => {
    const current = issuedLauncherContextRef.current;
    if (!current || (issued && !sameIssuedLauncherContext(current, issued))) {
      return false;
    }
    issuedLauncherContextRef.current = null;
    if (issuedLauncherContextExpiryTimerRef.current !== null) {
      window.clearTimeout(issuedLauncherContextExpiryTimerRef.current);
      issuedLauncherContextExpiryTimerRef.current = null;
    }
    return true;
  }, []);

  /** Keep a successfully dispatched context revocable until the host TTL.
   * The SDK may consume it sooner; a later revoke is then a safe no-op. */
  const retainIssuedLauncherContext = useCallback((issued: IssuedLauncherContext, expiresInMs: number) => {
    const existing = issuedLauncherContextRef.current;
    if (existing && !sameIssuedLauncherContext(existing, issued)) {
      // This should be unreachable because every new confirmation invalidates
      // the prior generation first. Failing closed avoids overwriting a live
      // newer handle if an unexpected ordering ever violates that invariant.
      return false;
    }
    if (issuedLauncherContextExpiryTimerRef.current !== null) {
      window.clearTimeout(issuedLauncherContextExpiryTimerRef.current);
    }
    issuedLauncherContextRef.current = issued;
    const boundedExpiry = Number.isFinite(expiresInMs)
      ? Math.min(Math.max(Math.ceil(expiresInMs), 0) + 1_000, 61_000)
      : 61_000;
    issuedLauncherContextExpiryTimerRef.current = window.setTimeout(() => {
      if (sameIssuedLauncherContext(issuedLauncherContextRef.current, issued)) {
        issuedLauncherContextRef.current = null;
        issuedLauncherContextExpiryTimerRef.current = null;
      }
    }, boundedExpiry);
    return true;
  }, []);

  /** Cancel the renderer-session-only source and invalidate every async
   * continuation. It is deliberately called before hide/close/focus changes,
   * so a stale closure can never issue or invoke after the launcher has gone
   * away. */
  const invalidateLauncherContextHandoff = useCallback(() => {
    launcherContextGenerationRef.current += 1;
    launcherContextDispatchKeyRef.current = null;
    pendingLauncherContextDispatchRef.current = null;
    activeLauncherContextSurfaceRef.current = null;
    const issued = issuedLauncherContextRef.current;
    clearIssuedLauncherContextReference(issued);
    revokeIssuedLauncherContext(issued);
    setPluginCenterLauncherContext(null);
    setPendingLauncherContextDispatch(null);
  }, [clearIssuedLauncherContextReference, revokeIssuedLauncherContext]);

  const launcherOpen = surface === "launcher";
  const pluginCenterOpen = surface === "plugin-center";
  const toolboxOpen = surface === "toolbox";
  // A recorder can only finalize after MediaRecorder emits `onstop`. Keep its
  // hidden drawer mounted long enough to save the final WebM when the user
  // returns to Spotlight or hides the window mid-recording.
  const toolboxMounted = toolboxOpen || toolboxRecordingPhase !== "idle";
  const settingsOpen = surface === "settings";
  const searchRuntimePlugins = useMemo(
    () => plugins.filter((plugin) =>
      plugin.enabled !== false
      && Boolean(plugin.frontendEntry)
      && Array.isArray(plugin.searchProviders)
      && plugin.searchProviders.length > 0
      && requestedSearchRuntimePluginIds.includes(plugin.id)
      // Exactly one iframe runtime owns a plugin at a time. The visible
      // surface takes over when a provider result or command opens it.
      && plugin.id !== activePlugin?.id,
    ),
    [activePlugin?.id, plugins, requestedSearchRuntimePluginIds],
  );

  // Plugin lifecycle changes are persisted by the native host, then reflected
  // through the canonical list. Tear down a visible iframe immediately when
  // its plugin is disabled or uninstalled, rather than leaving stale UI open
  // until the user manually closes it.
  useEffect(() => {
    if (!activePlugin) {
      return;
    }
    const current = plugins.find((plugin) => plugin.id === activePlugin.id);
    if (!current || current.enabled === false) {
      if (
        pendingLauncherContextDispatchRef.current?.plugin.id === activePlugin.id
        || activeLauncherContextSurfaceRef.current?.pluginId === activePlugin.id
      ) {
        invalidateLauncherContextHandoff();
      }
      setActivePlugin(null);
      setPendingPluginEvent((pending) =>
        pending?.pluginId === activePlugin.id ? null : pending,
      );
      setSurface((currentSurface) => currentSurface === "plugin" ? "launcher" : currentSurface);
      return;
    }
    if (
      pluginFrontendSourceKey(current) !== pluginFrontendSourceKey(activePlugin)
      && (
        pendingLauncherContextDispatchRef.current?.plugin.id === activePlugin.id
        || activeLauncherContextSurfaceRef.current?.pluginId === activePlugin.id
      )
    ) {
      // A refresh can retain the same plugin id while replacing its frontend
      // source. The new iframe must not inherit a handoff confirmed for the
      // prior source/lease.
      invalidateLauncherContextHandoff();
    }
    if (current !== activePlugin) {
      setActivePlugin(current);
    }
  }, [activePlugin, invalidateLauncherContextHandoff, plugins]);

  useEffect(() => {
    if (!isDesktop()) {
      return;
    }
    const window = getCurrentWindow();
    const height = surface === "plugin-center" ? 602 : 504;
    // Reopening is centered by the native resident shell. While the surface
    // remains visible, resizing a secondary panel must preserve a user's
    // deliberate drag position instead of snapping it back to the monitor.
    let disposed = false;
    void (async () => {
      try {
        await window.setSize(new LogicalSize(800, height));
        // Native setSize keeps the top-left fixed on common platforms. When a
        // person has not dragged this visible session, recenter after the
        // resize so opening the taller center feels like a single Spotlight
        // surface rather than a page that jumps downwards.
        if (!disposed && !windowWasDraggedThisRevealRef.current && surface !== "hidden") {
          await command("center_launcher_window");
        }
      } catch {
        // Browser previews and platforms that decline a native resize or
        // center action remain fully usable.
      }
    })();
    return () => {
      disposed = true;
    };
  }, [surface]);

  const contentResults = useMemo(
    () => findLauncherContentResults(query, quickNotes, clipboardHistory),
    [clipboardHistory, query, quickNotes],
  );
  const registeredSearchProviders = useMemo(
    () => new Set(registeredSearchProviderKeys),
    [registeredSearchProviderKeys],
  );
  const results = useMemo(() => {
    return mergeLauncherSearchResults(
      query,
      launcherCalculationResults(query),
      launcherSystemCommandResults(query),
      builtinToolResults(query),
      pluginCommandResults(plugins, query),
      pluginSearchResults,
      contentResults,
      searchResults,
    );
  }, [contentResults, pluginSearchResults, plugins, query, searchResults]);
  const spotlightSearchResults = useMemo<SpotlightLauncherItem[]>(
    () => results.map((result) => spotlightItemForSearchResult(
      result,
      nativeIconForResult(searchIconCache, result)
        ?? nativeIconForResult(homeIconCache, result),
    )),
    [homeIconCache, results, searchIconCache],
  );
  const pastedFileItems = useMemo<SpotlightLauncherItem[]>(
    () => pastedFileResults.map((result) => spotlightItemForSearchResult(result)),
    [pastedFileResults],
  );
  const launcherContextActions = useMemo(
    () => availableLauncherContextActions(
      deriveLauncherContextActions({
        query,
        pastedFiles: pastedFileResults.flatMap((result) => result.path
          ? [{
            kind: result.kind === "folder" ? "folder" as const : "file" as const,
            name: result.name,
            path: result.path,
          }]
          : []),
        hasPastedImage: Boolean(pastedImage),
        pastedImageType: pastedImage?.type,
      }),
      plugins,
    ),
    [pastedFileResults, pastedImage, plugins, query],
  );
  const launcherContextActionItems = useMemo<SpotlightLauncherItem[]>(
    () => launcherContextActions.map(spotlightItemForContextAction),
    [launcherContextActions],
  );
  const launcherContextActionById = useMemo<Map<string, LauncherContextAction>>(
    () => new Map(launcherContextActions.map((action) => [action.id, action] as const)),
    [launcherContextActions],
  );
  const pluginCenterLauncherContextPreview = useMemo(
    () => pluginCenterLauncherContext
      ? previewLauncherContextHandoff(pluginCenterLauncherContext)
      : null,
    [pluginCenterLauncherContext],
  );
  const launcherShortcutItems = useMemo<SpotlightLauncherItem[]>(
    () => launcherShortcuts.map((shortcut) =>
      spotlightItemForLauncherShortcut(
        shortcut,
        nativeIconForLauncherShortcut(homeIconCache, shortcut.id),
      )),
    [homeIconCache, launcherShortcuts],
  );
  const recentApplicationItems = useMemo(
    () => recentApplications.map((result) =>
      spotlightItemForSearchResult(
        result,
        nativeIconForResult(homeIconCache, result)
          ?? nativeIconForResult(searchIconCache, result),
      )),
    [homeIconCache, recentApplications, searchIconCache],
  );
  const pluginCommandItems = useMemo(() => plugins.flatMap((plugin) => {
      if (plugin.enabled === false || !Array.isArray(plugin.commands)) {
        return [];
      }
      return plugin.commands
        .filter((command): command is PluginCommandInfo => Boolean(command?.id))
        .map((pluginCommand) => ({
          id: `plugin-command:${plugin.id}:${pluginCommand.id}`,
          label: pluginCommand.name || pluginCommand.id,
          detail: [plugin.name, pluginCommand.description].filter(Boolean).join(" · ") || undefined,
          badge: plugin.name,
          icon: Puzzle,
          tone: "violet" as const,
        }));
    }), [plugins]);
  const launcherBaseItems = useMemo(
    () => [
      ...builtinPinnedItems,
      ...defaultMarketplaceItems,
      ...pluginCommandItems,
      ...recentApplicationItems,
      ...launcherShortcutItems,
    ],
    [launcherShortcutItems, pluginCommandItems, recentApplicationItems],
  );

  const launcherItemById = useMemo(() => {
    return buildLauncherItemIndex(launcherBaseItems, spotlightSearchResults);
  }, [launcherBaseItems, spotlightSearchResults]);
  const pinnedItems = useMemo(
    () => [
      ...pinnedItemIds.flatMap((id) => {
        const item = launcherItemById.get(id);
        return item ? [item] : [];
      }),
      ...launcherShortcutItems,
    ],
    [launcherItemById, launcherShortcutItems, pinnedItemIds],
  );
  const recentItems = useMemo(
    () => recentItemIds
      .map((id) => launcherItemById.get(id))
      .filter((item): item is SpotlightLauncherItem => Boolean(item)),
    [launcherItemById, recentItemIds],
  );
  const launcherHotkeyPresentation = useMemo(
    () => describeLauncherHotkey(
      health?.launcherHotkey,
      isDesktop(),
      health?.platform ?? "windows",
    ),
    [health?.launcherHotkey, health?.platform],
  );
  const launcherStatus = useMemo(() => {
    const count = new Intl.NumberFormat().format(status.indexedFiles);
    if (!isDesktop()) {
      return "浏览器预览";
    }
    const indexStatus = (() => {
      if (status.phase === "scanning") {
        return status.lastIndexedAt
          ? `正在重新扫描 · ${count} 项`
          : `正在建立索引 · ${count} 项`;
      }
      if (status.phase === "error") {
        return "本地索引需要注意";
      }
      return `本地索引已就绪 · ${count} 项`;
    })();

    return [indexStatus, launcherHotkeyPresentation.footerText]
      .filter((item): item is string => Boolean(item))
      .join(" · ");
  }, [launcherHotkeyPresentation.footerText, status.indexedFiles, status.lastIndexedAt, status.phase]);

  const showToast = useCallback((message: string) => {
    setToast(message);
    window.setTimeout(() => setToast((current) => (current === message ? null : current)), 3600);
  }, []);

  useEffect(() => {
    if (!isRecordingLauncherHotkey) {
      return;
    }

    const recordLauncherHotkey = (event: KeyboardEvent) => {
      event.preventDefault();
      event.stopPropagation();
      if (event.repeat) {
        return;
      }
      if (event.code === "Escape") {
        setIsRecordingLauncherHotkey(false);
        setLauncherHotkeyError(null);
        return;
      }

      const normalized = normalizeLauncherHotkey(event);
      if (!normalized.ok) {
        if (normalized.reason !== "modifier-only") {
          setLauncherHotkeyError(launcherHotkeyRejectionMessage(normalized.reason));
        }
        return;
      }

      setLauncherHotkeyDraft(normalized.accelerator);
      setLauncherHotkeyError(null);
      setIsRecordingLauncherHotkey(false);
    };

    window.addEventListener("keydown", recordLauncherHotkey, true);
    return () => window.removeEventListener("keydown", recordLauncherHotkey, true);
  }, [isRecordingLauncherHotkey]);

  useEffect(() => {
    if (settingsOpen) {
      return;
    }
    setIsRecordingLauncherHotkey(false);
    setLauncherHotkeyDraft(null);
    setLauncherHotkeyError(null);
  }, [settingsOpen]);

  useEffect(() => {
    if (!pendingLauncherContextDispatch) {
      return;
    }
    // The selected source remains in memory only while a just-opened plugin
    // surface registers its command. No host token exists during this wait.
    const timeout = window.setTimeout(() => {
      const current = pendingLauncherContextDispatchRef.current;
      if (
        current?.handoff.id !== pendingLauncherContextDispatch.handoff.id
        || current.generation !== pendingLauncherContextDispatch.generation
      ) {
        return;
      }
      invalidateLauncherContextHandoff();
      showToast("插件没有及时准备好接收这次上下文；未共享任何内容。请重新选择操作。");
    }, 15_000);
    return () => window.clearTimeout(timeout);
  }, [invalidateLauncherContextHandoff, pendingLauncherContextDispatch, showToast]);

  const refreshLauncherShortcuts = useCallback(async () => {
    if (!isDesktop()) {
      setLauncherShortcuts([]);
      return;
    }
    try {
      const shortcuts = await command<LauncherShortcutView[]>("list_launcher_shortcuts");
      setLauncherShortcuts(shortcuts);
    } catch (error) {
      setLauncherShortcuts([]);
      showToast(error instanceof Error ? error.message : "无法读取文件启动项。");
    }
  }, [showToast]);

  const togglePinnedItem = useCallback((item: SpotlightLauncherItem) => {
    const shortcutId = shortcutIdFromLauncherItemId(item.id);
    if (shortcutId) {
      if (!isDesktop()) {
        showToast("文件启动项只在 iHub 桌面版中管理。");
        return;
      }
      void (async () => {
        try {
          await command<boolean>("unpin_launcher_shortcut", { shortcutId });
          setLauncherShortcuts((current) => current.filter((shortcut) => shortcut.id !== shortcutId));
          setRecentItemIds((current) => current.filter((id) => id !== item.id));
          setSearchResults((current) => current.map((result) => result.pinnedShortcutId === shortcutId
            ? { ...result, pinnedShortcutId: undefined }
            : result));
          showToast(`已取消固定“${item.label}”。`);
        } catch (error) {
          showToast(error instanceof Error ? error.message : "无法取消固定该文件启动项。");
        }
      })();
      return;
    }

    const sourceResult = results.find((result) => result.id === item.id);
    const sourceShortcutId = sourceResult?.pinnedShortcutId;
    if (sourceShortcutId) {
      if (!isDesktop()) {
        showToast("文件启动项只在 iHub 桌面版中管理。");
        return;
      }
      void (async () => {
        try {
          await command<boolean>("unpin_launcher_shortcut", { shortcutId: sourceShortcutId });
          setLauncherShortcuts((current) => current.filter((shortcut) => shortcut.id !== sourceShortcutId));
          setSearchResults((current) => current.map((result) => result.id === sourceResult?.id
            ? { ...result, pinnedShortcutId: undefined }
            : result));
          showToast(`已取消固定“${item.label}”。`);
        } catch (error) {
          showToast(error instanceof Error ? error.message : "无法取消固定该文件启动项。");
        }
      })();
      return;
    }

    if (sourceResult?.pinEligible) {
      if (!isDesktop()) {
        showToast("文件启动固定只在 iHub 桌面版中可用。");
        return;
      }
      void (async () => {
        try {
          const shortcut = await command<LauncherShortcutView>("pin_launcher_shortcut_from_search", {
            searchId: sourceResult.id,
          });
          setLauncherShortcuts((current) => [
            shortcut,
            ...current.filter((candidate) => candidate.id !== shortcut.id),
          ]);
          setSearchResults((current) => current.map((result) => result.id === sourceResult.id
            ? { ...result, pinnedShortcutId: shortcut.id }
            : result));
          showToast(`已固定“${shortcut.name}”；它会显示在启动页的“已固定”中。`);
        } catch (error) {
          showToast(error instanceof Error ? error.message : "无法固定该文件启动项。");
        }
      })();
      return;
    }

    if (sourceResult && (sourceResult.kind === "file" || sourceResult.kind === "folder" || sourceResult.kind === "application")) {
      showToast("该文件或应用当前不支持安全固定；请从可固定的本地搜索结果中操作。");
      return;
    }
    if (!launcherItemById.has(item.id)) {
      showToast("该项目当前不能固定；请从已安装工具或最近应用中操作。");
      return;
    }
    const isPinned = pinnedItemIds.includes(item.id);
    setPinnedItemIds((current) => isPinned
      ? current.filter((id) => id !== item.id)
      : [item.id, ...current.filter((id) => id !== item.id)].slice(0, 30));
    showToast(isPinned ? `已取消固定“${item.label}”。` : `已固定“${item.label}”。`);
  }, [launcherItemById, pinnedItemIds, results, showToast]);

  const handlePastedFiles = useCallback(async () => {
    if (!isDesktop()) {
      showToast("文件粘贴需要 iHub 桌面版读取系统剪贴板。");
      return;
    }
    try {
      const files = await command<ClipboardFile[]>("read_clipboard_files");
      const seen = new Set<string>();
      const next = files.flatMap((file, index) => {
        if (!file.path || !file.name || seen.has(file.path)) {
          return [];
        }
        seen.add(file.path);
        return [{
          id: `clipboard-file:${file.path}`,
          name: file.name,
          path: file.path,
          kind: file.kind === "folder" ? "folder" as const : "file" as const,
          score: 1_000 - index,
          metadata: `已粘贴 · ${file.path}`,
        }];
      });
      if (!next.length) {
        showToast("剪贴板中的文件已不存在或无法读取。");
        return;
      }
      setPastedImage(null);
      setPastedFileResults(next);
      setQuery("");
      showToast(`已接收 ${next.length} 个文件；选择后按 Enter 打开。`);
    } catch (error) {
      showToast(error instanceof Error ? error.message : "无法读取剪贴板中的文件。");
    }
  }, [showToast]);

  const handlePastedImage = useCallback((image: LauncherPastedImage) => {
    setPastedFileResults([]);
    setPastedImage(image);
    setQuery("");
    showToast("已接收图片；请选择一个上下文操作。图片不会自动保存或交给插件。");
  }, [showToast]);

  const handleNativePastedImage = useCallback(async () => {
    if (!isDesktop()) {
      showToast("图片粘贴需要 iHub 桌面版读取系统剪贴板。");
      return;
    }
    try {
      const image = await command<ClipboardImage | null>("read_clipboard_image");
      if (!image) {
        showToast("剪贴板中没有可读取的图片。");
        return;
      }
      const response = await fetch(image.dataUrl);
      const blob = await response.blob();
      if (!blob.size) {
        throw new Error("剪贴板图片为空。");
      }
      handlePastedImage({
        blob,
        name: image.name || "ihub-pasted-image.png",
        type: blob.type || image.mimeType || "image/png",
      });
    } catch (error) {
      showToast(error instanceof Error ? error.message : "无法读取剪贴板中的图片。");
    }
  }, [handlePastedImage, showToast]);

  const clearPastedImage = useCallback(() => {
    setPastedImage(null);
  }, []);

  const refreshStatus = useCallback(async () => {
    if (!isDesktop()) {
      return;
    }

    try {
      const [nextStatus, nextHealth] = await Promise.all([
        command<IndexStatus>("get_index_status"),
        command<AppHealth>("get_app_health"),
      ]);
      setStatus(nextStatus);
      setHealth(nextHealth);
      setAutostartEnabled(nextHealth.autostart ?? null);
    } catch (error) {
      setStatus((current) => ({
        ...current,
        phase: "error",
        message: error instanceof Error ? error.message : "无法读取索引状态",
      }));
    }
  }, []);

  const refreshPlugins = useCallback(async () => {
    if (!isDesktop()) {
      return;
    }

    try {
      setPlugins(await command<PluginInfo[]>("list_plugins"));
    } catch (error) {
      showToast(error instanceof Error ? error.message : "无法读取插件列表。");
    }
  }, [showToast]);

  /**
   * Update handles are short-lived native resources. A check owns at most one
   * of them, and every replacement or unmount closes the older handle. The
   * promise ref is the concurrency guard: UI state updates are asynchronous,
   * so checking `updatePhase` alone would still permit two rapid clicks.
  */
  const installDiscoveredUpdate = useCallback(async (
    update: Update,
    origin: UpdateInstallOrigin,
  ): Promise<boolean> => {
    // A check can replace and close its Update handle. Do not consume one
    // until that check is completely finished, and never install an older
    // handle that has since been replaced by a newer discovery result.
    if (!canInstallDiscoveredUpdate({
      automaticAttemptedVersions: autoInstallAttemptedVersionsRef.current,
      automaticEnabled: autoInstallSignedUpdatesRef.current,
      candidateIsCurrent: updateRef.current === update,
      checkInFlight: updateCheckInFlightRef.current !== null,
      developmentBuild: isDevelopmentBuild,
      desktop: isDesktop(),
      discoveryActive: updateDiscoveryActiveRef.current,
      installed: updateInstalledRef.current,
      installInFlight: updateInstallInFlightRef.current,
      origin,
      version: update.version,
    })) {
      return false;
    }

    if (origin === "automatic") {
      // Mark a version before downloading: a broken package must remain
      // available for a deliberate manual retry, but must not be retried by
      // the six-hour automatic discovery loop forever.
      const retainedAttempts = recordAutomaticUpdateAttempt(
        autoInstallAttemptedVersionsRef.current,
        update.version,
      );
      autoInstallAttemptedVersionsRef.current = new Set(retainedAttempts);
      persistLauncherValue(autoInstallAttemptedVersionsStorageKey, retainedAttempts);
    }

    updateInstallInFlightRef.current = true;
    setUpdateError(null);
    setUpdateProgress({ received: 0 });
    setUpdatePhase("downloading");

    try {
      await update.downloadAndInstall((event) => {
        if (!updateDiscoveryActiveRef.current) {
          return;
        }
        if (event.event === "Started") {
          setUpdateProgress({
            received: 0,
            total: event.data.contentLength,
          });
          setUpdatePhase("downloading");
        } else if (event.event === "Progress") {
          setUpdateProgress((current) => ({
            ...current,
            received: current.received + event.data.chunkLength,
          }));
        } else {
          setUpdatePhase("installing");
        }
      });

      if (updateRef.current === update) {
        updateRef.current = null;
      }
      updateInstalledRef.current = true;
      void update.close().catch(() => undefined);
      if (updateDiscoveryActiveRef.current) {
        setAvailableUpdate(null);
        setUpdatePhase("installed");
        showToast("更新已安装；重启 iHub 后生效。");
      }
      return true;
    } catch (error) {
      const message = error instanceof Error ? error.message : "更新安装失败。";
      if (updateDiscoveryActiveRef.current) {
        setUpdateError(message);
        setUpdatePhase("error");
        showToast(message);
      }
      return false;
    } finally {
      updateInstallInFlightRef.current = false;
    }
  }, [showToast]);

  const checkForUpdates = useCallback((): Promise<void> => {
    if (
      !isDesktop()
      || isDevelopmentBuild
      || !updateDiscoveryActiveRef.current
      || updateInstallInFlightRef.current
      || updateInstalledRef.current
    ) {
      return Promise.resolve();
    }

    const activeRequest = updateCheckInFlightRef.current;
    if (activeRequest) {
      return activeRequest;
    }

    let discoveredUpdate: Update | null = null;
    const request = (async () => {
      setUpdatePhase("checking");
      setUpdateError(null);
      try {
        const update = await check();
        if (!updateDiscoveryActiveRef.current) {
          void update?.close().catch(() => undefined);
          return;
        }

        const previousUpdate = updateRef.current;
        if (update) {
          discoveredUpdate = update;
          updateRef.current = update;
          setAvailableUpdate(update);
          setUpdatePhase("available");
          if (previousUpdate && previousUpdate !== update) {
            void previousUpdate.close().catch(() => undefined);
          }
        } else {
          updateRef.current = null;
          setAvailableUpdate(null);
          setUpdatePhase("idle");
          void previousUpdate?.close().catch(() => undefined);
        }
      } catch (error) {
        // Keep signed-update source failures visible in Settings instead of
        // silently presenting an idle state as if the check succeeded.
        if (updateDiscoveryActiveRef.current) {
          setUpdatePhase("error");
          setUpdateError(error instanceof Error ? error.message : "无法检查发行更新。");
        }
      }
    })();

    updateCheckInFlightRef.current = request;
    void request.finally(() => {
      if (updateCheckInFlightRef.current === request) {
        updateCheckInFlightRef.current = null;
        // This is deliberately after clearing the check guard. The helper
        // rechecks the live opt-in ref, so disabling the switch while this
        // request was in flight prevents automatic installation.
        if (discoveredUpdate) {
          void installDiscoveredUpdate(discoveredUpdate, "automatic");
        }
      }
    });
    return request;
  }, [installDiscoveredUpdate]);

  const markSearchProviderRegistered = useCallback((pluginId: string, providerId: string) => {
    const isDeclared = plugins.some((plugin) =>
      plugin.id === pluginId
      && plugin.enabled !== false
      && plugin.searchProviders?.some((provider) => provider.id === providerId),
    );
    if (!isDeclared) {
      return;
    }
    const key = pluginSearchProviderKey(pluginId, providerId);
    setRegisteredSearchProviderKeys((current) =>
      current.includes(key) ? current : [...current, key],
    );
  }, [plugins]);

  const unmarkSearchProvider = useCallback((pluginId: string, providerId: string) => {
    const key = pluginSearchProviderKey(pluginId, providerId);
    setRegisteredSearchProviderKeys((current) => {
      const next = current.filter((entry) => entry !== key);
      return next.length === current.length ? current : next;
    });
  }, []);

  const clearPluginSearchProviders = useCallback((pluginId: string) => {
    const prefix = `${pluginId}:`;
    setRegisteredSearchProviderKeys((current) => {
      const next = current.filter((key) => !key.startsWith(prefix));
      return next.length === current.length ? current : next;
    });
  }, []);

  useEffect(() => {
    const declared = new Set(
      plugins.filter((plugin) => plugin.enabled !== false).flatMap((plugin) => plugin.searchProviders?.map((provider) =>
        pluginSearchProviderKey(plugin.id, provider.id),
      ) ?? []),
    );
    setRegisteredSearchProviderKeys((current) => {
      const next = current.filter((key) => declared.has(key));
      return next.length === current.length ? current : next;
    });
    const pluginIds = new Set(
      plugins.filter((plugin) => plugin.enabled !== false).map((plugin) => plugin.id),
    );
    setRequestedSearchRuntimePluginIds((current) => {
      const next = current.filter((pluginId) => pluginIds.has(pluginId));
      return next.length === current.length ? current : next;
    });
    setPluginSearchResults((current) => {
      const next = current.filter((result) => !result.pluginId || pluginIds.has(result.pluginId));
      return next.length === current.length ? current : next;
    });
  }, [plugins]);

  const refreshQuickNotes = useCallback(() => {
    setQuickNotes(readLauncherQuickNotes());
  }, []);

  const requestPluginSearch = useCallback(async (nextQuery: string, requestId: number) => {
    if (!isDesktop()) {
      if (requestId === searchRequestRef.current) {
        setPluginSearchResults([]);
      }
      return;
    }

    const declaredProviders = eligiblePluginSearchProviders(plugins, nextQuery);
    const nextRuntimeIds = [...new Set(declaredProviders.map((provider) => provider.plugin.id))];
    setRequestedSearchRuntimePluginIds((current) =>
      current.length === nextRuntimeIds.length
      && current.every((pluginId, index) => pluginId === nextRuntimeIds[index])
        ? current
        : nextRuntimeIds,
    );
    const providers = declaredProviders.filter((provider) =>
      registeredSearchProviders.has(pluginSearchProviderKey(provider.plugin.id, provider.provider.id)),
    );
    if (providers.length === 0) {
      if (requestId === searchRequestRef.current) {
        setPluginSearchResults([]);
      }
      return;
    }

    const responses = await Promise.allSettled(
      providers.map((provider) => command<PluginSearchResponse>("query_plugin_search", {
        pluginId: provider.plugin.id,
        providerId: provider.provider.id,
        query: provider.query,
        limit: MAX_LAUNCHER_RESULTS_PER_PROVIDER,
        context: { source: "launcher" },
      })),
    );
    if (requestId !== searchRequestRef.current) {
      return;
    }

    const nextResults = responses.flatMap((response, index) => {
      if (response.status !== "fulfilled") {
        // A provider timeout or failure is isolated to that one iframe. The
        // primary native/local search must remain smooth and actionable.
        return [];
      }
      const provider = providers[index];
      if (
        response.value.pluginId !== provider.plugin.id
        || response.value.providerId !== provider.provider.id
      ) {
        return [];
      }
      return pluginProviderResponseResults(response.value, provider);
    })
      .sort((left, right) => right.score - left.score)
      .slice(0, MAX_LAUNCHER_PLUGIN_PROVIDERS * MAX_LAUNCHER_RESULTS_PER_PROVIDER);
    setPluginSearchResults(nextResults);
  }, [plugins, registeredSearchProviders]);

  const requestSearch = useCallback(async (nextQuery: string, requestId: number) => {
    const nextQuickNotes = readLauncherQuickNotes();
    if (!isDesktop()) {
      if (requestId === searchRequestRef.current) {
        setSearchResults(filterPreviewResults(nextQuery));
        setQuickNotes(nextQuickNotes);
        // Do not invent clipboard history in the browser preview.
        setClipboardHistory(null);
      }
      return;
    }

    void requestPluginSearch(nextQuery, requestId);

    const [searchResponse, clipboardResponse] = await Promise.allSettled([
      command<SearchResult[]>("search_entries", {
        query: nextQuery,
        limit: 12,
      }),
      command<ClipboardHistorySnapshot>("get_clipboard_history", { limit: 60 }),
    ]);

    if (requestId !== searchRequestRef.current) {
      return;
    }

    setQuickNotes(nextQuickNotes);
    if (searchResponse.status === "fulfilled") {
      const nativeResults = searchResponse.value;
      setSearchResults(nativeResults);
      const searchResultIds = nativeResults
        .filter((result) => (
          result.kind === "application"
          || result.kind === "file"
          || result.kind === "folder"
        ))
        .map((result) => result.id)
        .slice(0, 12);
      if (searchResultIds.length > 0) {
        void requestSystemIconMap(searchResultIds, [])
          .then((icons) => {
            if (requestId === searchRequestRef.current) {
              setSearchIconCache((current) =>
                mergeNativeIconCache(current, icons, nativeResults));
            }
          })
          .catch(() => {
            // A transparent reserved slot is preferable to a false app glyph.
          });
      }
    } else {
      setSearchResults([]);
      showToast(
        searchResponse.reason instanceof Error
          ? searchResponse.reason.message
          : "搜索引擎暂不可用。",
      );
    }

    if (clipboardResponse.status === "fulfilled") {
      setClipboardHistory(clipboardResponse.value);
    } else {
      // A failed optional history read must not retain stale text in results.
      setClipboardHistory(null);
    }
  }, [requestPluginSearch, showToast]);

  useEffect(() => {
    const requestId = searchRequestRef.current + 1;
    searchRequestRef.current = requestId;
    const normalizedQuery = query.trim();
    if (surface === "launcher") {
      refreshQuickNotes();
    }
    if (surface !== "launcher" || !normalizedQuery) {
      setSearchResults([]);
      setPluginSearchResults([]);
      setRequestedSearchRuntimePluginIds((current) => (current.length === 0 ? current : []));
      if (!isDesktop()) {
        setClipboardHistory(null);
      }
      return;
    }
    // Results from a previous plugin request must never be shown under the
    // next keystroke while its bounded provider call is still in flight.
    setPluginSearchResults([]);
    const timer = window.setTimeout(() => {
      void requestSearch(query, requestId);
    }, 55);

    return () => window.clearTimeout(timer);
  }, [query, refreshQuickNotes, requestSearch, surface]);

  useEffect(() => {
    const onStorage = (event: StorageEvent) => {
      if (event.key === quickNotesStorageKey) {
        refreshQuickNotes();
      }
    };
    window.addEventListener("storage", onStorage);
    return () => window.removeEventListener("storage", onStorage);
  }, [refreshQuickNotes]);

  useEffect(() => {
    persistLauncherValue(launcherRecentStorageKey, recentItemIds);
  }, [recentItemIds]);

  useEffect(() => {
    persistLauncherValue(launcherPinnedStorageKey, pinnedItemIds);
  }, [pinnedItemIds]);

  useEffect(() => {
    persistLauncherValue(launcherRecentApplicationsStorageKey, recentApplications);
  }, [recentApplications]);

  useEffect(() => {
    const generation = homeIconRequestRef.current + 1;
    homeIconRequestRef.current = generation;
    if (!isDesktop()) {
      return;
    }
    const searchResultIds = recentApplications
      .filter((result) => result.kind === "application")
      .map((result) => result.id);
    const launcherShortcutIds = launcherShortcuts
      .filter((shortcut) => shortcut.status === "ready")
      .map((shortcut) => shortcut.id);
    if (searchResultIds.length === 0 && launcherShortcutIds.length === 0) {
      return;
    }

    void requestSystemIconMap(searchResultIds, launcherShortcutIds)
      .then((icons) => {
        if (generation === homeIconRequestRef.current) {
          setHomeIconCache((current) =>
            mergeNativeIconCache(
              current,
              icons,
              recentApplications,
              launcherShortcutIds,
            ));
        }
      })
      .catch(() => {
        // Keep already-rendered artwork stable when an optional refresh fails.
      });
  }, [launcherShortcuts, recentApplications]);

  useEffect(() => {
    persistLauncherValue(launcherShowRecentStorageKey, showRecent);
  }, [showRecent]);

  useEffect(() => {
    persistLauncherValue(launcherSpaceActivatesStorageKey, spaceActivates);
  }, [spaceActivates]);

  useEffect(() => {
    autoInstallSignedUpdatesRef.current = autoInstallSignedUpdates;
    // This preference only has meaning in a packaged desktop build. Browser
    // preview and the source-first development launcher deliberately never
    // enroll themselves in release installation.
    if (isDesktop() && !isDevelopmentBuild) {
      persistLauncherValue(autoInstallSignedUpdatesStorageKey, autoInstallSignedUpdates);
    }
  }, [autoInstallSignedUpdates]);

  useEffect(() => {
    if (booted.current) {
      return;
    }
    booted.current = true;
    updateDiscoveryActiveRef.current = true;

    let unlistenFocus: () => void = () => {};
    let unlistenHide: () => void = () => {};
    let statusInterval: number | undefined;
    let updateInterval: number | undefined;
    let disposed = false;
    const start = async () => {
      if (!isDesktop()) {
        return;
      }

      await Promise.all([refreshStatus(), refreshPlugins(), refreshLauncherShortcuts()]);
      if (disposed) {
        return;
      }
      try {
        unlistenFocus = await onFocusSearch(({ freshReveal }) => {
          if (freshReveal) {
            windowWasDraggedThisRevealRef.current = false;
            invalidateLauncherContextHandoff();
            refreshQuickNotes();
            setQuery("");
            setPastedFileResults([]);
            setPastedImage(null);
            setActivePlugin(null);
            setPendingPluginEvent(null);
            setSurface("launcher");
          }
          // A visible-but-unfocused launcher keeps its current query/plugin/
          // tool surface and only restores keyboard focus.
          setLauncherFocusSignal((current) => current + 1);
        });
        unlistenHide = await onHideSearch(() => {
          invalidateLauncherContextHandoff();
          setQuery("");
          setPastedFileResults([]);
          setPastedImage(null);
          setActivePlugin(null);
          setPendingPluginEvent(null);
          setSurface("hidden");
        });
      } catch {
        // A manual click remains available if a global shortcut is unavailable.
      }
      if (disposed) {
        return;
      }

      if (isDevelopmentBuild) {
        // The development launcher deliberately runs the current worktree;
        // signed release updates belong only to packaged production builds.
        setUpdatePhase("idle");
        return;
      }

      statusInterval = window.setInterval(() => void refreshStatus(), 1800);
      // Production discovery is intentionally bounded: check once after the
      // desktop shell has booted, then no more than once every six hours.
      void checkForUpdates();
      updateInterval = window.setInterval(
        () => void checkForUpdates(),
        UPDATE_DISCOVERY_INTERVAL_MS,
      );
    };

    void start();
    return () => {
      disposed = true;
      updateDiscoveryActiveRef.current = false;
      unlistenFocus();
      unlistenHide();
      if (statusInterval !== undefined) {
        window.clearInterval(statusInterval);
      }
      if (updateInterval !== undefined) {
        window.clearInterval(updateInterval);
      }
      const retainedUpdate = updateRef.current;
      updateRef.current = null;
      void retainedUpdate?.close().catch(() => undefined);
    };
  }, [
    checkForUpdates,
    invalidateLauncherContextHandoff,
    refreshLauncherShortcuts,
    refreshPlugins,
    refreshQuickNotes,
    refreshStatus,
  ]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.isComposing) {
        return;
      }
      if ((event.metaKey || event.ctrlKey) && event.key.toLocaleLowerCase() === "k") {
        event.preventDefault();
        invalidateLauncherContextHandoff();
        setPastedFileResults([]);
        setPastedImage(null);
        setActivePlugin(null);
        setPendingPluginEvent(null);
        setToolboxLaunchContext(null);
        setSurface("launcher");
        setLauncherFocusSignal((current) => current + 1);
        return;
      }
      if ((event.metaKey || event.ctrlKey) && event.key === ",") {
        event.preventDefault();
        invalidateLauncherContextHandoff();
        setSurface("settings");
        return;
      }
      if (event.key === "Escape") {
        if (surface === "plugin") {
          invalidateLauncherContextHandoff();
          setActivePlugin(null);
          setPendingPluginEvent(null);
          setSurface("launcher");
          setLauncherFocusSignal((current) => current + 1);
        } else if (surface === "plugin-center" || surface === "toolbox" || surface === "settings") {
          invalidateLauncherContextHandoff();
          setToolboxLaunchContext(null);
          setSurface("launcher");
          setLauncherFocusSignal((current) => current + 1);
        } else if (surface === "launcher" && query.trim()) {
          setQuery("");
        } else if (surface === "launcher") {
          invalidateLauncherContextHandoff();
          setSurface("hidden");
          if (isDesktop()) {
            void getCurrentWindow().hide().catch(() => undefined);
          }
        }
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [invalidateLauncherContextHandoff, query, surface]);

  const refreshIndex = async () => {
    if (!isDesktop()) {
      showToast("桌面版会在后台索引你选择的位置。");
      return;
    }

    setIsRefreshing(true);
    try {
      await command<void>("index_default_roots");
      await refreshStatus();
      showToast("已开始重新扫描；上次完整索引会继续用于搜索。");
    } catch (error) {
      showToast(error instanceof Error ? error.message : "无法刷新索引。");
    } finally {
      setIsRefreshing(false);
    }
  };

  const setIndexRoots = async (roots: string[]) => {
    if (!isDesktop()) {
      throw new Error("浏览器预览不会修改本地索引目录。");
    }

    setIsRefreshing(true);
    try {
      const nextStatus = await command<IndexStatus>("set_index_roots", { roots });
      setStatus(nextStatus);
    } finally {
      setIsRefreshing(false);
    }
  };

  const installUpdate = async () => {
    if (!isDesktop()) {
      showToast("浏览器预览不会下载更新；请在 iHub 桌面端执行此操作。");
      return;
    }
    const update = updateRef.current ?? availableUpdate;
    if (!update) {
      showToast("当前没有可安装的更新。");
      return;
    }
    await installDiscoveredUpdate(update, "manual");
  };

  const toggleAutoInstallSignedUpdates = () => {
    if (!isDesktop() || isDevelopmentBuild || updateInstalledRef.current) {
      return;
    }

    // Mutate the ref synchronously as well as state. A network check can
    // settle before React commits this event, and its completion must observe
    // a just-disabled switch rather than the previous render's value.
    const nextEnabled = !autoInstallSignedUpdatesRef.current;
    autoInstallSignedUpdatesRef.current = nextEnabled;
    setAutoInstallSignedUpdates(nextEnabled);
  };

  const toggleAutostart = async () => {
    if (!isDesktop()) {
      showToast("浏览器预览不会修改开机自启动；请在 iHub 桌面端设置。");
      return;
    }

    const nextEnabled = !(autostartEnabled ?? health?.autostart ?? false);
    setAutostartError(null);
    setIsUpdatingAutostart(true);
    try {
      const result = await command<AutostartStatus>("set_autostart", {
        enabled: nextEnabled,
      });
      setAutostartEnabled(result.enabled);
      setHealth((current) =>
        current ? { ...current, autostart: result.enabled } : current,
      );
      showToast(result.enabled ? "开机自启动已启用。" : "开机自启动已关闭。");
    } catch (error) {
      const message = error instanceof Error ? error.message : "无法更新开机自启动设置。";
      setAutostartError(message);
      showToast(message);
    } finally {
      setIsUpdatingAutostart(false);
    }
  };

  const beginLauncherHotkeyRecording = () => {
    if (!isDesktop()) {
      showToast("浏览器预览不会注册系统级快捷键；请在 iHub 桌面端设置。");
      return;
    }
    if (isUpdatingLauncherHotkey) {
      return;
    }
    if (isRecordingLauncherHotkey) {
      setIsRecordingLauncherHotkey(false);
      setLauncherHotkeyError(null);
      return;
    }
    setLauncherHotkeyDraft(null);
    setLauncherHotkeyError(null);
    setIsRecordingLauncherHotkey(true);
  };

  const applyLauncherHotkey = async () => {
    if (!isDesktop() || !launcherHotkeyDraft || isUpdatingLauncherHotkey) {
      return;
    }

    setLauncherHotkeyError(null);
    setIsUpdatingLauncherHotkey(true);
    try {
      const result = await command<LauncherHotkeyStatus>("set_launcher_hotkey", {
        accelerator: launcherHotkeyDraft,
      });
      setHealth((current) => current
        ? { ...current, launcherHotkey: result }
        : current);
      const label = formatLauncherHotkey(
        result.accelerator ?? launcherHotkeyDraft,
        health?.platform ?? "windows",
      );
      setLauncherHotkeyDraft(null);
      showToast(`启动快捷键已改为 ${label}。`);
      if (!health) {
        await refreshStatus();
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : "无法更新启动快捷键。";
      setLauncherHotkeyError(message);
      showToast(message);
    } finally {
      setIsUpdatingLauncherHotkey(false);
    }
  };

  const resetLauncherHotkey = async () => {
    if (!isDesktop() || isUpdatingLauncherHotkey) {
      return;
    }

    setIsRecordingLauncherHotkey(false);
    setLauncherHotkeyError(null);
    setIsUpdatingLauncherHotkey(true);
    try {
      const result = await command<LauncherHotkeyStatus>("reset_launcher_hotkey");
      setHealth((current) => current
        ? { ...current, launcherHotkey: result }
        : current);
      setLauncherHotkeyDraft(null);
      showToast(launcherHotkeyResetAction(result).successMessage);
      if (!health) {
        await refreshStatus();
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : "无法恢复默认启动快捷键。";
      setLauncherHotkeyError(message);
      showToast(message);
    } finally {
      setIsUpdatingLauncherHotkey(false);
    }
  };

  const quitApplication = async () => {
    if (!isDesktop()) {
      showToast("浏览器预览不会退出桌面进程。");
      return;
    }
    try {
      await command<void>("quit_app");
    } catch (error) {
      const message = error instanceof Error ? error.message : "无法退出 iHub。";
      showToast(message);
    }
  };

  const updateActionLabel = (() => {
    if (updatePhase === "downloading") {
      if (updateProgress.total) {
        return `下载 ${Math.min(100, Math.round((updateProgress.received / updateProgress.total) * 100))}%`;
      }
      return "正在下载";
    }
    if (updatePhase === "installing") {
      return "正在安装";
    }
    if (updatePhase === "installed") {
      return "重启生效";
    }
    if (updatePhase === "error") {
      return "重试更新";
    }
    return availableUpdate ? `更新至 v${availableUpdate.version}` : "更新可用";
  })();

  const updateWorkInProgress = ["checking", "downloading", "installing"].includes(updatePhase);
  const hasUpdateAction = Boolean(availableUpdate) && !updateWorkInProgress && updatePhase !== "installed";
  const canCheckForUpdates = isDesktop() && !isDevelopmentBuild && updatePhase !== "installed";
  const canAutoInstallSignedUpdates = isDesktop() && !isDevelopmentBuild && updatePhase !== "installed";
  const autoInstallSignedUpdatesEnabled = canAutoInstallSignedUpdates && autoInstallSignedUpdates;
  const autostartIsEnabled = autostartEnabled ?? health?.autostart ?? false;
  const launcherHotkeyPlatform = health?.platform ?? "windows";
  const launcherHotkeyDraftLabel = launcherHotkeyDraft
    ? formatLauncherHotkey(launcherHotkeyDraft, launcherHotkeyPlatform)
    : null;
  const hotkeyResetAction = launcherHotkeyResetAction(health?.launcherHotkey);

  const returnToLauncher = () => {
    invalidateLauncherContextHandoff();
    refreshQuickNotes();
    setActivePlugin(null);
    setPendingPluginEvent(null);
    setToolboxLaunchContext(null);
    setPluginCenterInitialSearch(null);
    setSurface("launcher");
    setLauncherFocusSignal((current) => current + 1);
  };

  const dismissLauncher = async () => {
    invalidateLauncherContextHandoff();
    setQuery("");
    setPastedFileResults([]);
    setPastedImage(null);
    setToolboxLaunchContext(null);
    setPluginCenterInitialSearch(null);
    setSurface("hidden");
    if (isDesktop()) {
      try {
        await getCurrentWindow().hide();
      } catch {
        // Browser previews intentionally have no native window to dismiss.
      }
    }
  };

  const openToolbox = (
    tab: ToolboxTab,
    launchContext?: Omit<ToolboxLaunchContext, "requestId">,
  ) => {
    invalidateLauncherContextHandoff();
    setToolboxTab(tab);
    setToolboxLaunchContext(launchContext
      ? { ...launchContext, requestId: ++toolboxContextRequestRef.current }
      : null);
    setSurface("toolbox");
  };

  const openPluginCenter = () => {
    invalidateLauncherContextHandoff();
    setPluginCenterInitialSearch(null);
    setSurface("plugin-center");
  };

  const beginPluginLauncherContextHandoff = (
    category: "text" | "files" | "image",
    suggestedUse: string,
  ) => {
    let handoff: LauncherContextHandoff | null = null;
    if (category === "text") {
      const text = query.trim();
      if (text) {
        handoff = { id: nextLauncherContextUiId(), kind: "text", suggestedUse, text };
      }
    } else if (category === "files") {
      const files = pastedFileResults.flatMap((result) => result.path
        ? [{
          path: result.path,
          name: result.name,
          kind: result.kind === "folder" ? "folder" as const : "file" as const,
        }]
        : []);
      if (files.length > 16) {
        showToast("一次最多可交接 16 个已粘贴文件或文件夹；请缩小选择后重试。");
        return;
      }
      if (files.length) {
        handoff = { id: nextLauncherContextUiId(), kind: "files", suggestedUse, files };
      }
    } else if (pastedImage) {
      handoff = {
        id: nextLauncherContextUiId(),
        kind: "image",
        suggestedUse,
        image: pastedImage,
      };
    }

    if (!handoff) {
      showToast("当前可交接内容已变化或不可用；未共享任何内容。");
      return;
    }
    // Starting a new explicit selection supersedes any previous selection or
    // half-ready plugin surface before we retain the fresh in-memory source.
    invalidateLauncherContextHandoff();
    setPluginCenterInitialSearch(null);
    setPluginCenterLauncherContext(handoff);
    setSurface("plugin-center");
  };

  const openSettings = () => {
    invalidateLauncherContextHandoff();
    setSurface("settings");
  };

  const recordRecent = (item: SpotlightLauncherItem) => {
    if (!launcherItemById.has(item.id) || !isLauncherRecentDestination(item.id)) {
      return;
    }
    setRecentItemIds((current) =>
      retainLauncherRecent([item.id, ...current.filter((id) => id !== item.id)]));
  };

  // The avatar is navigation chrome, so entering the center must not displace
  // a real application or tool from the recent-work history.
  const openPluginCenterFromLauncher = () => {
    openPluginCenter();
  };

  const requestPluginLauncherContextHandoff = (
    plugin: PluginInfo,
    pluginCommand: PluginCommandInfo,
  ) => {
    const handoff = pluginCenterLauncherContext;
    if (!handoff) {
      showToast("这次上下文已取消；未共享任何内容。");
      return;
    }
    if (!isDesktop()) {
      showToast("插件上下文交接只在 iHub 桌面版中可用。未共享任何内容。");
      return;
    }
    const currentPlugin = plugins.find((candidate) => candidate.id === plugin.id) ?? plugin;
    const eligibleCommand = eligibleLauncherContextCommands([currentPlugin], handoff)
      .find(({ command: candidate }) => candidate.id === pluginCommand.id)?.command;
    const sourceKey = pluginFrontendSourceKey(currentPlugin);
    if (!eligibleCommand || !sourceKey) {
      showToast("该插件命令不再声明所需的上下文权限；未共享任何内容。");
      return;
    }

    // This is the point at which a person has made the second, concrete
    // choice. We still do not create a host token: the fresh iframe must
    // finish lifecycle.ready and command registration first.
    invalidateLauncherContextHandoff();
    const pending: PendingLauncherContextDispatch = {
      handoff,
      plugin: currentPlugin,
      command: eligibleCommand,
      generation: launcherContextGenerationRef.current,
      pluginSourceKey: sourceKey,
    };
    setPluginCenterInitialSearch(null);
    setPendingPluginEvent(null);
    pendingLauncherContextDispatchRef.current = pending;
    setPendingLauncherContextDispatch(pending);
    setActivePlugin(currentPlugin);
    setSurface("plugin");
    showToast(`正在准备“${currentPlugin.name} / ${eligibleCommand.name || eligibleCommand.id}”…`);
  };

  const dispatchLauncherContextAfterSurfaceReady = useCallback((pluginId: string, frontendLeaseId: string) => {
    const pending = pendingLauncherContextDispatchRef.current;
    const currentPlugin = pluginsRef.current.find((plugin) => plugin.id === pluginId) ?? null;
    if (!pending || pending.plugin.id !== pluginId) {
      return;
    }
    if (
      pending.plugin.enabled === false
      || !currentPlugin
      || pluginFrontendSourceKey(currentPlugin) !== pending.pluginSourceKey
    ) {
      // Do not retain a selected source while a same-id plugin refresh is
      // acquiring another lease. A later lifecycle.ready belongs to new code
      // and requires a brand-new, visible user confirmation.
      invalidateLauncherContextHandoff();
      return;
    }
    const dispatchKey = [
      pending.generation,
      pending.handoff.id,
      pending.plugin.id,
      pending.command.id,
      frontendLeaseId,
    ].join(":");
    if (launcherContextDispatchKeyRef.current === dispatchKey) {
      return;
    }
    launcherContextDispatchKeyRef.current = dispatchKey;
    const activeSurface: ActiveLauncherContextSurface = {
      pluginId,
      leaseId: frontendLeaseId,
      generation: pending.generation,
      pluginSourceKey: pending.pluginSourceKey,
    };
    activeLauncherContextSurfaceRef.current = activeSurface;
    // Clear the renderer-side sensitive reference before asynchronous native
    // work begins. The local closure is the one explicit, user-confirmed use.
    pendingLauncherContextDispatchRef.current = null;
    setPendingLauncherContextDispatch(null);

    const isStillCurrent = () => {
      const active = activeLauncherContextSurfaceRef.current;
      const latestPlugin = pluginsRef.current.find((plugin) => plugin.id === pluginId) ?? null;
      return (
        launcherContextGenerationRef.current === pending.generation
        && active?.pluginId === pluginId
        && active?.leaseId === frontendLeaseId
        && active?.generation === pending.generation
        && active?.pluginSourceKey === pending.pluginSourceKey
        && pluginFrontendSourceKey(latestPlugin) === pending.pluginSourceKey
      );
    };

    void (async () => {
      let issued: PluginLauncherContextIssue | null = null;
      let issuedRecord: IssuedLauncherContext | null = null;
      try {
        if (!isStillCurrent()) {
          return;
        }
        const context = await hostRequestForLauncherContext(pending.handoff);
        if (!isStillCurrent()) {
          return;
        }
        issued = await command<PluginLauncherContextIssue>("issue_plugin_launcher_context", {
          pluginId: pending.plugin.id,
          commandId: pending.command.id,
          context,
          frontendLeaseId,
        });
        const candidate: IssuedLauncherContext = {
          pluginId: pending.plugin.id,
          contextId: issued.contextId,
          generation: pending.generation,
          leaseId: frontendLeaseId,
          pluginSourceKey: pending.pluginSourceKey,
        };
        if (!isStillCurrent()) {
          // This chain became stale while the host was issuing. Revoke only
          // its local result; never write or clear the global handle because
          // a newer confirmation may already own it.
          revokeIssuedLauncherContext(candidate);
          return;
        }
        if (!retainIssuedLauncherContext(candidate, issued.expiresInMs)) {
          // Failing closed here protects a live newer handle if an impossible
          // interleaving attempts to overwrite it.
          revokeIssuedLauncherContext(candidate);
          return;
        }
        issuedRecord = candidate;
        await command<string>("invoke_plugin_frontend_command", {
          pluginId: pending.plugin.id,
          commandId: pending.command.id,
          input: null,
          context: null,
          launcherContextId: issued.contextId,
          frontendLeaseId,
        });
        if (!isStillCurrent()) {
          // The event may have been emitted immediately before cancellation;
          // revoke the still-host-owned one-shot record when it was not yet
          // consumed. The identity check cannot touch a newer handoff.
          if (issuedRecord) {
            revokeIssuedLauncherContext(issuedRecord);
            clearIssuedLauncherContextReference(issuedRecord);
          }
          return;
        }
        // Keep `issuedRecord` in the ref after a successful emit. The token
        // remains consumable until the plugin consumes it or the host TTL;
        // hide/Escape/iframe failure must still be able to revoke it.
        showToast(`已向“${pending.plugin.name}”发送一次${pending.handoff.kind === "text" ? "文本" : pending.handoff.kind === "files" ? "文件元数据" : "图片元数据"}交接。`);
      } catch (error) {
        if (issued) {
          // A command can disappear between lifecycle.ready and emit (for
          // example after a source update). Remove the in-memory payload now
          // instead of leaving it alive until its normal short expiry.
          const candidate = issuedRecord ?? {
            pluginId: pending.plugin.id,
            contextId: issued.contextId,
            generation: pending.generation,
            leaseId: frontendLeaseId,
            pluginSourceKey: pending.pluginSourceKey,
          };
          revokeIssuedLauncherContext(candidate);
          clearIssuedLauncherContextReference(candidate);
        }
        if (launcherContextGenerationRef.current === pending.generation) {
          showToast(error instanceof Error ? error.message : "无法交接该上下文；未共享任何内容。");
        }
      } finally {
        if (launcherContextDispatchKeyRef.current === dispatchKey) {
          launcherContextDispatchKeyRef.current = null;
        }
      }
    })();
  }, [
    clearIssuedLauncherContextReference,
    invalidateLauncherContextHandoff,
    retainIssuedLauncherContext,
    revokeIssuedLauncherContext,
    showToast,
  ]);

  const discardPendingLauncherContextForSurface = useCallback((pluginId: string, leaseId?: string) => {
    const pending = pendingLauncherContextDispatchRef.current;
    const active = activeLauncherContextSurfaceRef.current;
    if (pending?.plugin.id !== pluginId && active?.pluginId !== pluginId) {
      return;
    }
    // An error from an old iframe must not cancel a newer source for the same
    // plugin id. Before readiness there is no active lease yet, so the plugin
    // match remains sufficient to cancel that exact waiting surface.
    if (leaseId && active && (active.pluginId !== pluginId || active.leaseId !== leaseId)) {
      return;
    }
    invalidateLauncherContextHandoff();
  }, [invalidateLauncherContextHandoff]);

  const activateResult = async (result?: SearchResult, recentItem?: SpotlightLauncherItem) => {
    if (!result) {
      return;
    }

    const recordSuccessfulAction = () => {
      const candidate = recentItem
        ?? (result.commandId ? launcherItemById.get(result.commandId) : undefined)
        ?? launcherItemById.get(result.id);
      if (candidate) {
        recordRecent(candidate);
      }
      if (result.kind === "application" && result.path) {
        setRecentApplications((current) => retainLauncherRecent([
          result,
          ...current.filter((item) => item.id !== result.id),
        ]));
      }
    };

    if (result.commandId === "ihub.index.default") {
      await refreshIndex();
      return;
    }

    if (result.commandId === "ihub.open-settings") {
      openSettings();
      return;
    }

    const toolTab = toolboxTabForCommand(result.commandId);
    if (toolTab) {
      openToolbox(
        toolTab,
        toolTab === "calculator" && result.calculatorExpression
          ? { calculatorInput: result.calculatorExpression }
          : toolTab === "time" && result.timeInput
            ? { timeInput: result.timeInput }
            : undefined,
      );
      recordSuccessfulAction();
      return;
    }

    if (!isDesktop()) {
      showToast("这是界面预览；在 iHub 桌面端中执行此操作。");
      return;
    }

    try {
      if ((result.kind === "file" || result.kind === "folder" || result.kind === "application") && result.path) {
        await command<void>("open_path", { path: result.path });
        recordSuccessfulAction();
        await dismissLauncher();
      } else if (
        result.pluginId
        && result.pluginProviderId
        && result.pluginSearchResultId
      ) {
        const plugin = plugins.find((item) => item.id === result.pluginId);
        if (!plugin?.frontendEntry) {
          showToast("该插件搜索结果没有可用的前端入口。");
          return;
        }
        // A provider result is data, never executable code. The existing
        // iframe bridge receives it as a generic SDK event after activation;
        // the plugin decides whether to render, copy, or ignore it.
        invalidateLauncherContextHandoff();
        setPendingPluginEvent(createFrontendSearchSelectionEvent(
          plugin.id,
          result.pluginProviderId,
          result.pluginSearchResultId,
          result.pluginPayload,
        ));
        setActivePlugin(plugin);
        setSurface("plugin");
        recordSuccessfulAction();
        return;
      } else if (result.pluginId && result.commandId) {
        const plugin = plugins.find((item) => item.id === result.pluginId);
        const pluginCommand = Array.isArray(plugin?.commands)
          ? plugin.commands.find((command): command is PluginCommandInfo => command?.id === result.commandId)
          : undefined;
        if (pluginCommand?.execution === "frontend" || (plugin?.frontendEntry && !plugin.hasNativeWorker)) {
          if (!plugin?.frontendEntry) {
            showToast("该插件命令需要前端入口，但当前插件没有可用界面。");
            return;
          }
          invalidateLauncherContextHandoff();
          setPendingPluginEvent(createFrontendCommandEvent(plugin.id, result.commandId));
          setActivePlugin(plugin);
          setSurface("plugin");
          recordSuccessfulAction();
          return;
        }
        if (!plugin?.hasNativeWorker) {
          showToast("该插件没有可运行的原生 worker 或前端命令入口。");
          return;
        }
        // A source-lock commit is part of a native execution approval. A
        // routine update can preserve a package version, but it must never
        // inherit a previous binary confirmation for different code.
        const approvalKey = [
          plugin.id,
          plugin.version,
          plugin.sourceLock?.resolvedCommit ?? plugin.commit ?? plugin.localPath ?? "local",
        ].join("@");
        if (!approvedNativePlugins.current.has(approvalKey)) {
          const approved = window.confirm(
            `“${plugin.name}” 将启动本机二进制 worker。\n\n原生插件不受沙箱限制，只应运行你信任的发布者。是否继续？`,
          );
          if (!approved) {
            showToast("已取消启动原生插件。你可以在确认来源后再次执行。");
            return;
          }
          approvedNativePlugins.current.add(approvalKey);
        }
        const nativeResult = await command<PluginCommandResult>("run_plugin_command", {
          pluginId: result.pluginId,
          commandId: result.commandId,
        });
        if (!nativeResult.success) {
          const detail = nativePluginCommandSummary({
            ...nativeResult,
            stdout: nativeResult.stderr || nativeResult.stdout,
          });
          throw new Error(detail || `原生插件执行失败（退出码 ${nativeResult.exitCode ?? "未知"}）。`);
        }
        recordSuccessfulAction();
        const detail = nativePluginCommandSummary(nativeResult);
        showToast(detail ? `${plugin.name}：${detail}` : `${plugin.name} 已完成。`);
        await dismissLauncher();
      }
    } catch (error) {
      showToast(error instanceof Error ? error.message : "无法执行该项目。");
    }
  };

  const activateLauncherShortcut = async (shortcutId: string, item: SpotlightLauncherItem) => {
    if (!isDesktop()) {
      showToast("这是界面预览；文件启动只在 iHub 桌面端中执行。");
      return;
    }
    try {
      // The renderer supplies only the UUID. Rust resolves the current index
      // source, revalidates the live filesystem target, then invokes the
      // system opener without accepting a persistent path from the WebView.
      await command<void>("open_launcher_shortcut", { shortcutId });
      recordRecent(item);
      await dismissLauncher();
    } catch (error) {
      showToast(error instanceof Error ? error.message : "无法打开该文件启动项。");
    }
  };

  const activateContextAction = (action: LauncherContextAction) => {
    if (action.target.kind === "plugin-handoff") {
      beginPluginLauncherContextHandoff(action.target.category, action.target.suggestedUse);
      return;
    }

    const tab = toolboxTabForCommand(action.target.commandId);
    if (!tab) {
      showToast("该上下文操作当前没有可用的内置工具入口。");
      return;
    }
    const launchContext = action.target.jsonInput !== undefined
      ? { jsonInput: action.target.jsonInput }
      : action.target.renameDirectory !== undefined
        ? { renameDirectory: action.target.renameDirectory }
        : undefined;
    openToolbox(tab, launchContext);
  };

  const activateSpotlightItem = (item: SpotlightLauncherItem) => {
    const contextAction = launcherContextActionById.get(item.id);
    if (contextAction) {
      activateContextAction(contextAction);
      return;
    }
    const shortcutId = shortcutIdFromLauncherItemId(item.id);
    if (shortcutId) {
      void activateLauncherShortcut(shortcutId, item);
      return;
    }
    if (item.id === "ihub.open-plugin-center") {
      openPluginCenter();
      return;
    }
    if (item.id === "ihub.open-settings") {
      openSettings();
      return;
    }

    const toolTab = toolboxTabForCommand(item.id);
    if (toolTab) {
      openToolbox(toolTab);
      recordRecent(item);
      return;
    }
    if (item.id === "ihub.open-developer-tools") {
      openToolbox("developer");
      recordRecent(item);
      return;
    }

    const result = results.find((candidate) => candidate.id === item.id)
      ?? results.find((candidate) => candidate.commandId === item.id)
      ?? pastedFileResults.find((candidate) => candidate.id === item.id)
      ?? recentApplications.find((candidate) => candidate.id === item.id);
    if (result) {
      void activateResult(result, item);
    }
  };

  const startLauncherWindowDrag = useCallback(async () => {
    if (!isDesktop()) {
      return;
    }
    try {
      windowWasDraggedThisRevealRef.current = true;
      await getCurrentWindow().startDragging();
    } catch {
      // Browser preview and platforms that decline a native drag should keep
      // the launcher fully usable; dragging is only an optional shell affordance.
    }
  }, []);

  return (
    <main className="app-shell app-shell--spotlight">
      <SpotlightLauncher
        focusSignal={launcherFocusSignal}
        onActivate={activateSpotlightItem}
        onClose={() => void dismissLauncher()}
        onOpenPluginCenter={openPluginCenterFromLauncher}
        onOpenSettings={openSettings}
        onUnavailableItem={(item) => showToast(`“${item.label}”当前不可用；请重新搜索后固定，或右键取消固定。`)}
        onClearPastedItems={() => setPastedFileResults([])}
        onStartWindowDrag={startLauncherWindowDrag}
        onPasteFiles={handlePastedFiles}
        onPasteImage={handlePastedImage}
        onPasteNativeImage={handleNativePastedImage}
        onQueryChange={setQuery}
        onTogglePinned={togglePinnedItem}
        onToggleRecent={() => setShowRecent((current) => !current)}
        open={launcherOpen}
        contextActions={launcherContextActionItems}
        pinnedItemIds={pinnedItemIds}
        pinnedItems={pinnedItems}
        query={query}
        pastedItems={pastedFileItems}
        recentItems={recentItems}
        searchResults={spotlightSearchResults}
        showRecent={showRecent}
        spaceActivates={spaceActivates}
        statusText={launcherStatus}
      />

      <AnimatePresence>
        {settingsOpen ? (
          <>
            <motion.button
              aria-label="关闭设置"
              className="settings-scrim"
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              exit={{ opacity: 0 }}
              onClick={returnToLauncher}
              type="button"
            />
            <motion.aside
              aria-labelledby="settings-title"
              className="settings-panel"
              initial={prefersReducedMotion ? false : { opacity: 0, y: -8, scale: 0.98 }}
              animate={prefersReducedMotion ? undefined : { opacity: 1, y: 0, scale: 1 }}
              exit={prefersReducedMotion ? undefined : { opacity: 0, y: -6, scale: 0.985 }}
              role="dialog"
              transition={{ duration: 0.18, ease: [0.16, 1, 0.3, 1] }}
            >
              <div className="settings-panel__header">
                <div>
                  <span>APPLICATION</span>
                  <h2 id="settings-title">偏好设置</h2>
                </div>
                <button
                  aria-label="关闭设置"
                  className="icon-button"
                  onClick={returnToLauncher}
                  type="button"
                >
                  <X size={16} />
                </button>
              </div>

              <section className="settings-section" aria-labelledby="updates-title">
                <div className="settings-section__icon">
                  {updatePhase === "installed" ? <Check size={16} /> : <Download size={16} />}
                </div>
                <div className="settings-section__copy">
                  <h3 id="updates-title">发行更新</h3>
                  <p>
                    {!isDesktop()
                      ? "浏览器预览不会查询或安装发行更新。"
                      : isDevelopmentBuild
                        ? "开发启动器始终运行当前源码；签名正式版才检查发行更新。"
                      : updatePhase === "installed"
                        ? "已安装，重启 iHub 后生效。"
                        : updatePhase === "checking"
                            ? "正在检查签名更新…"
                            : updatePhase === "error"
                              ? "无法检查发行更新；请检查网络或稍后重试。"
                              : availableUpdate
                                ? autoInstallSignedUpdatesEnabled
                                  ? `发现 v${availableUpdate.version}，已启用自动安装。`
                                  : `发现 v${availableUpdate.version}，自动安装已关闭，可手动安装。`
                                : "启动时及之后每 6 小时会检查已签名的发行更新。"}
                  </p>
                  <p>
                    {isDesktop() && !isDevelopmentBuild
                      ? "仅接收版本更高且签名验证通过的 iHub 正式发行版，不拉取开发源码或插件。Windows 会移交系统安装程序，iHub 可能关闭并重新打开；macOS 在下次启动时应用。"
                      : "自动安装仅适用于打包后的 iHub 桌面正式版。"}
                  </p>
                  {canCheckForUpdates ? (
                    <button
                      aria-busy={updatePhase === "checking"}
                      className="settings-action"
                      disabled={updateWorkInProgress}
                      onClick={() => void checkForUpdates()}
                      type="button"
                    >
                      {updatePhase === "checking" ? (
                        <LoaderCircle className="spin" size={14} />
                      ) : (
                        <RefreshCw size={14} />
                      )}
                      {updatePhase === "checking" ? "正在检查" : "立即检查"}
                    </button>
                  ) : null}
                  {availableUpdate ? (
                    <button
                      className="settings-action"
                      disabled={!hasUpdateAction}
                      onClick={() => void installUpdate()}
                      type="button"
                    >
                      {updatePhase === "downloading" || updatePhase === "installing" ? (
                        <LoaderCircle className="spin" size={14} />
                      ) : (
                        <Download size={14} />
                      )}
                      {updateActionLabel}
                    </button>
                  ) : null}
                  {updatePhase === "downloading" ? (
                    <div className="update-progress" aria-label={updateActionLabel}>
                      <span
                        style={{
                          width: updateProgress.total
                            ? `${Math.min(100, (updateProgress.received / updateProgress.total) * 100)}%`
                            : "18%",
                        }}
                      />
                    </div>
                  ) : null}
                  {updateError ? (
                    <p className="settings-error" role="alert">
                      <CircleAlert size={13} />
                      {updateError}
                    </p>
                  ) : null}
                </div>
                <button
                  aria-label={autoInstallSignedUpdatesEnabled ? "关闭自动安装已签名正式版" : "开启自动安装已签名正式版"}
                  aria-pressed={autoInstallSignedUpdatesEnabled}
                  className={"settings-switch" + (autoInstallSignedUpdatesEnabled ? " is-on" : "")}
                  disabled={!canAutoInstallSignedUpdates}
                  onClick={toggleAutoInstallSignedUpdates}
                  type="button"
                >
                  <span />
                </button>
              </section>

              <section className="settings-section" aria-labelledby="autostart-title">
                <div className="settings-section__icon">
                  <Power size={16} />
                </div>
                <div className="settings-section__copy">
                  <h3 id="autostart-title">开机自启动</h3>
                  <p>
                    {!isDesktop()
                      ? "浏览器预览仅展示此选项，不会更改系统设置。"
                      : isUpdatingAutostart
                        ? "正在更新系统启动项…"
                        : autostartEnabled === null
                        ? "正在读取系统启动项…"
                        : autostartIsEnabled
                          ? "已启用：登录后 iHub 会在后台就绪。"
                          : "已关闭：需要时从应用程序中手动启动。"}
                  </p>
                  {autostartError ? (
                    <p className="settings-error" role="alert">
                      <CircleAlert size={13} />
                      {autostartError}
                    </p>
                  ) : null}
                </div>
                <button
                  aria-label={autostartIsEnabled ? "关闭开机自启动" : "启用开机自启动"}
                  aria-busy={isUpdatingAutostart}
                  aria-pressed={autostartIsEnabled}
                  className={"settings-switch" + (autostartIsEnabled ? " is-on" : "")}
                  disabled={isUpdatingAutostart}
                  onClick={() => void toggleAutostart()}
                  type="button"
                >
                  <span />
                </button>
              </section>

              <section className="settings-section" aria-labelledby="launcher-hotkey-title">
                <div className="settings-section__icon">
                  <Keyboard size={16} />
                </div>
                <div className="settings-section__copy">
                  <h3 id="launcher-hotkey-title">启动快捷键</h3>
                  <p aria-label={launcherHotkeyPresentation.ariaLabel} aria-live="polite">
                    {launcherHotkeyPresentation.settingsDescription}
                  </p>
                  {launcherHotkeyPresentation.shortcutLabel ? (
                    <p className="settings-hotkey-current">
                      <span>当前</span>
                      <kbd>{launcherHotkeyPresentation.shortcutLabel}</kbd>
                    </p>
                  ) : null}
                  <div className="settings-hotkey-controls">
                    <button
                      aria-pressed={isRecordingLauncherHotkey}
                      className={"settings-hotkey-recorder" + (isRecordingLauncherHotkey ? " is-recording" : "")}
                      disabled={!isDesktop() || isUpdatingLauncherHotkey}
                      onClick={beginLauncherHotkeyRecording}
                      type="button"
                    >
                      {isUpdatingLauncherHotkey ? (
                        <LoaderCircle className="spin" size={13} />
                      ) : (
                        <Keyboard size={13} />
                      )}
                      {isRecordingLauncherHotkey
                        ? "请按组合键 · Esc 取消"
                        : launcherHotkeyDraftLabel
                          ? `已录制 ${launcherHotkeyDraftLabel}`
                          : isDesktop()
                            ? "录制新快捷键"
                            : "仅桌面端可设置"}
                    </button>
                    {launcherHotkeyDraftLabel ? (
                      <button
                        className="settings-hotkey-apply"
                        disabled={isUpdatingLauncherHotkey}
                        onClick={() => void applyLauncherHotkey()}
                        type="button"
                      >
                        <Check size={13} />
                        应用
                      </button>
                    ) : null}
                    {hotkeyResetAction.visible ? (
                      <button
                        className="settings-hotkey-reset"
                        disabled={isUpdatingLauncherHotkey}
                        onClick={() => void resetLauncherHotkey()}
                        type="button"
                      >
                        <RefreshCw size={12} />
                        {hotkeyResetAction.label}
                      </button>
                    ) : null}
                  </div>
                  {launcherHotkeyError ? (
                    <p className="settings-error" role="alert">
                      <CircleAlert size={13} />
                      {launcherHotkeyError}
                    </p>
                  ) : null}
                </div>
              </section>

              <section className="settings-section" aria-labelledby="recent-title">
                <div className="settings-section__icon">
                  <History size={16} />
                </div>
                <div className="settings-section__copy">
                  <h3 id="recent-title">搜索面板显示最近使用</h3>
                  <p>
                    可在主搜索框按 <kbd>Alt</kbd> + <kbd>H</kbd> 或双击搜索框快速切换。
                  </p>
                </div>
                <button
                  aria-label={showRecent ? "隐藏最近使用" : "显示最近使用"}
                  aria-pressed={showRecent}
                  className={"settings-switch" + (showRecent ? " is-on" : "")}
                  onClick={() => setShowRecent((current) => !current)}
                  type="button"
                >
                  <span />
                </button>
              </section>

              <section className="settings-section" aria-labelledby="space-title">
                <div className="settings-section__icon">
                  <Zap size={16} />
                </div>
                <div className="settings-section__copy">
                  <h3 id="space-title">空格键执行打开</h3>
                  <p>用方向键选择结果后，空格键与 Enter 一样执行；输入多词搜索时空格始终输入文本。</p>
                </div>
                <button
                  aria-label={spaceActivates ? "关闭空格键执行" : "启用空格键执行"}
                  aria-pressed={spaceActivates}
                  className={"settings-switch" + (spaceActivates ? " is-on" : "")}
                  onClick={() => setSpaceActivates((current) => !current)}
                  type="button"
                >
                  <span />
                </button>
              </section>

              <section className="settings-section" aria-labelledby="quit-title">
                <div className="settings-section__icon">
                  <LogOut size={16} />
                </div>
                <div className="settings-section__copy">
                  <h3 id="quit-title">退出 iHub</h3>
                  <p>结束驻留进程并释放全局快捷键。标题栏关闭、Esc 和失焦仍只隐藏启动器。</p>
                  <button
                    className="settings-action is-danger"
                    disabled={!isDesktop()}
                    onClick={() => void quitApplication()}
                    type="button"
                  >
                    <LogOut size={14} />
                    退出 iHub
                  </button>
                </div>
              </section>

              <p className="settings-panel__meta">
                {isDesktop() && health ? `iHub ${health.version} · ${health.platform}` : "iHub 浏览器预览"}
              </p>
            </motion.aside>
          </>
        ) : null}
      </AnimatePresence>

      {pluginCenterOpen ? (
        <Suspense fallback={null}>
          <PluginCenter
            initialSearch={pluginCenterInitialSearch}
            launcherContext={pluginCenterLauncherContextPreview}
            onClose={returnToLauncher}
            onOpenFrontend={(plugin) => {
              invalidateLauncherContextHandoff();
              setPendingPluginEvent(null);
              setActivePlugin(plugin);
              setSurface("plugin");
            }}
            onOpenBuiltinTool={(tool) => openToolbox(tool)}
            onOpenDeveloperTools={() => openToolbox("developer")}
            onOpenSettings={openSettings}
            onRequestLauncherContextHandoff={requestPluginLauncherContextHandoff}
            onStartWindowDrag={startLauncherWindowDrag}
            onPluginsChanged={setPlugins}
            onToast={showToast}
            open={pluginCenterOpen}
            plugins={plugins}
            hostTarget={health?.hostTarget}
          />
        </Suspense>
      ) : null}
      {toolboxMounted ? (
        <Suspense fallback={null}>
          <ToolboxDrawer
            activeTab={toolboxTab}
            indexStatus={status}
            isRefreshingIndex={isRefreshing}
            launchContext={toolboxLaunchContext}
            onClose={returnToLauncher}
            onRefreshIndex={() => void refreshIndex()}
            onSetIndexRoots={setIndexRoots}
            onTabChange={setToolboxTab}
            onToast={showToast}
            onPluginsChanged={setPlugins}
            onRecordingPhaseChange={setToolboxRecordingPhase}
            onPastedImageConsumed={clearPastedImage}
            open={toolboxOpen}
            pastedImage={pastedImage}
            plugins={plugins}
          />
        </Suspense>
      ) : null}
      {searchRuntimePlugins.length > 0 || activePlugin ? (
        <Suspense fallback={null}>
          {isDesktop() ? searchRuntimePlugins.map((plugin) => (
            <PluginFrontendFrame
              key={`search-runtime:${plugin.id}`}
              mode="runtime"
              onClose={() => undefined}
              onPendingEventHandled={() => undefined}
              onRuntimeDisposed={clearPluginSearchProviders}
              onSearchProviderRegistered={markSearchProviderRegistered}
              onSearchProviderUnregistered={unmarkSearchProvider}
              onToast={showToast}
              pendingEvent={null}
              plugin={plugin}
            />
          )) : null}
          <PluginFrontendFrame
            onClose={returnToLauncher}
            onPendingEventHandled={(eventId) => {
              setPendingPluginEvent((current) => (current?.id === eventId ? null : current));
            }}
            onRuntimeDisposed={clearPluginSearchProviders}
            onSearchProviderRegistered={markSearchProviderRegistered}
            onSearchProviderUnregistered={unmarkSearchProvider}
            onSurfaceReady={dispatchLauncherContextAfterSurfaceReady}
            onSurfaceUnavailable={discardPendingLauncherContextForSurface}
            onToast={showToast}
            pendingEvent={pendingPluginEvent}
            plugin={activePlugin}
          />
        </Suspense>
      ) : null}

      <AnimatePresence>
        {toast ? (
          <motion.div
            className="toast"
            initial={{ opacity: 0, y: 12, scale: 0.98 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 8, scale: 0.98 }}
            role="status"
          >
            {toast}
          </motion.div>
        ) : null}
      </AnimatePresence>
    </main>
  );
}
