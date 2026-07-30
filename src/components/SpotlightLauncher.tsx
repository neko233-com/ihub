import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import {
  AppWindow,
  Binary,
  BookOpenText,
  Braces,
  Calculator,
  Camera,
  ChevronLeft,
  CircleAlert,
  Clock3,
  Clipboard,
  Cloud,
  Code2,
  Command,
  Files,
  FolderSearch,
  NotebookPen,
  Palette,
  Puzzle,
  QrCode,
  Search,
  Sparkles,
  Video,
  X,
  type LucideIcon,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { launcherHomePreview } from "../lib/launcher-home";
import { launcherInputUsesHorizontalGridNavigation } from "../lib/launcher-input-navigation";
import {
  createLongPressWindowDragController,
  type LongPressWindowDragController,
  supportsLongPressWindowDrag,
  WINDOW_DRAG_LONG_PRESS_MS,
} from "../lib/window-drag-long-press";
import { BlurText } from "./BlurText";

/**
 * A host-owned launcher item. iHub deliberately does not manufacture entries
 * for arbitrary third-party applications or plugins: callers should pass
 * those only after they are genuinely available on this machine.
 */
export interface SpotlightLauncherItem {
  id: string;
  label: string;
  /** A compact second line, such as a command description or source. */
  detail?: string;
  /** Optional right-aligned status, e.g. "内置" or a real plugin version. */
  badge?: string;
  /** Built-in vector artwork. Non-native omissions use a neutral app glyph. */
  icon?: LucideIcon;
  /** Optional local or plugin-owned artwork; Lucide remains the built-in fallback. */
  iconSrc?: string;
  /** Keeps the native-app slot transparent until host-owned artwork is ready. */
  nativeIconPending?: boolean;
  /** Keeps the visual system restrained while making quick scanning easier. */
  tone?: "mint" | "violet" | "amber" | "blue" | "slate";
  disabled?: boolean;
  /** A host-validated local file/folder/application result may be right-click
   * pinned even while it appears in the transient search-result group. */
  canPinFromSearch?: boolean;
  /** The host may mark a current result as already pinned by an opaque ID. */
  pinnedShortcutId?: string;
  /** The host keeps an unavailable shortcut visible so it can be removed, but
   * it must never be activated as if its target still existed. */
  unavailable?: boolean;
}

/**
 * Tracks only the application slots currently waiting on one bounded native
 * icon request. A matching generation is cleared in `finally`, so an empty,
 * partial, or failed host response cannot leave Spotlight permanently blank.
 */
export interface SpotlightNativeIconPendingBatch {
  generation: number;
  searchResultIds: ReadonlySet<string>;
  launcherShortcutIds: ReadonlySet<string>;
}

export function createSpotlightNativeIconPendingBatch(
  generation: number,
  searchResultIds: readonly string[],
  launcherShortcutIds: readonly string[],
): SpotlightNativeIconPendingBatch | null {
  const searchIds = new Set(searchResultIds.filter(Boolean));
  const shortcutIds = new Set(launcherShortcutIds.filter(Boolean));
  return searchIds.size || shortcutIds.size
    ? {
        generation,
        searchResultIds: searchIds,
        launcherShortcutIds: shortcutIds,
      }
    : null;
}

export function settleSpotlightNativeIconPendingBatch(
  current: SpotlightNativeIconPendingBatch | null,
  generation: number,
): SpotlightNativeIconPendingBatch | null {
  return current?.generation === generation ? null : current;
}

/** A bitmap explicitly pasted into the launcher's search field. */
export interface LauncherPastedImage {
  blob: Blob;
  name: string;
  type: string;
}

export interface SpotlightLauncherProps {
  /** Keeps the launcher mounted only while the native launcher window is visible. */
  open: boolean;
  /** Increment this when a native shortcut should move focus back into the field. */
  focusSignal?: number;
  onClose: () => void;
  /** Receives item activation; the host decides how a built-in command runs. */
  onActivate?: (item: SpotlightLauncherItem) => void;
  /** Optional controlled query, suitable for forwarding to the native search engine. */
  query?: string;
  onQueryChange?: (query: string) => void;
  /** Real search results supplied by the host. They replace the three groups during a query. */
  searchResults?: readonly SpotlightLauncherItem[];
  /** Context-aware actions for text, pasted files, or a pasted bitmap. The
   * host owns both their classification and activation. */
  contextActions?: readonly SpotlightLauncherItem[];
  /** Empty by default so the UI never pretends that an app was used recently. */
  recentItems?: readonly SpotlightLauncherItem[];
  /** Mirrors uTools' searchable-panel preference for showing recent items. */
  showRecent?: boolean;
  /** Alt+H toggles recent items without replacing native text-selection gestures. */
  onToggleRecent?: () => void;
  /** uTools-compatible preference: Space can execute the selected result. */
  spaceActivates?: boolean;
  /** Defaults to genuine iHub built-ins, not third-party plugins. */
  pinnedItems?: readonly SpotlightLauncherItem[];
  /** Persisted item IDs let right-click pin state stay visible outside the fixed row. */
  pinnedItemIds?: readonly string[];
  /** Desktop-friendly pin control. Host-validated local results may also opt
   * in while they are shown in the transient search group. */
  onTogglePinned?: (item: SpotlightLauncherItem) => void;
  /** Gives the host a chance to explain why a visible but stale fixed item
   * cannot be opened. */
  onUnavailableItem?: (item: SpotlightLauncherItem) => void;
  /** Ephemeral, host-validated file entries from the current paste action. */
  pastedItems?: readonly SpotlightLauncherItem[];
  onClearPastedItems?: () => void;
  onPasteFiles?: () => void | Promise<void>;
  onPasteImage?: (image: LauncherPastedImage) => void;
  /** Native fallback for bitmap clipboards WebView does not expose as a File. */
  onPasteNativeImage?: () => void | Promise<void>;
  /** Defaults to first-party entry points only. */
  marketplaceItems?: readonly SpotlightLauncherItem[];
  /** Optional short, factual status near the bottom edge. */
  statusText?: string;
  onOpenPluginCenter?: () => void;
  onOpenSettings?: () => void;
  /** Invoked after a deliberate long press in the reserved top drag zone. */
  onStartWindowDrag?: () => void | Promise<void>;
}

interface LauncherGroup {
  id: "context" | "recent" | "pinned" | "marketplace" | "pasted" | "search";
  label: string;
  /** Optional factual affordance shown at the far edge of the group title. */
  countLabel?: string;
  /** Full filtered count, independent of the bounded home preview. */
  totalCount?: number;
  /** Only the uTools-style recent/pinned affordances may open a focused group
   * view. Other counts remain factual, non-navigational labels. */
  expandable?: boolean;
  emptyLabel: string;
  items: readonly SpotlightLauncherItem[];
}

type LauncherExpandableGroupId = "recent" | "pinned";

function isExpandableLauncherGroup(
  group: LauncherGroup,
): group is LauncherGroup & { id: LauncherExpandableGroupId; expandable: true } {
  return group.expandable === true && (group.id === "recent" || group.id === "pinned");
}

function launcherSelectionKey(groupId: LauncherGroup["id"], itemId: string) {
  return `${groupId}:${itemId}`;
}

export const builtinPinnedItems: readonly SpotlightLauncherItem[] = [
  {
    id: "ihub.tool.local-search",
    label: "本地搜索",
    detail: "索引中的文件与文件夹",
    icon: FolderSearch,
    tone: "mint",
    badge: "内置",
  },
  {
    id: "ihub.tool.color",
    label: "取色器",
    detail: "HEX、RGB、HSL",
    icon: Palette,
    tone: "violet",
    badge: "内置",
  },
  {
    id: "ihub.tool.screenshot",
    label: "截图",
    detail: "选择区域并导出 PNG",
    icon: Camera,
    tone: "blue",
    badge: "内置",
  },
  {
    id: "ihub.tool.clipboard-history",
    label: "剪贴板历史",
    detail: "本机文本记录",
    icon: Clipboard,
    tone: "mint",
    badge: "内置",
  },
  {
    id: "ihub.tool.json",
    label: "JSON",
    detail: "校验与格式化",
    icon: Braces,
    tone: "amber",
    badge: "内置",
  },
  {
    id: "ihub.tool.quick-note",
    label: "快速便签",
    detail: "本机保存与搜索",
    icon: NotebookPen,
    tone: "violet",
    badge: "内置",
  },
  {
    id: "ihub.tool.convert",
    label: "进制转换",
    detail: "数值与文本编码",
    icon: Binary,
    tone: "amber",
    badge: "内置",
  },
  {
    id: "ihub.tool.calculator",
    label: "计算器",
    detail: "四则、括号与幂运算",
    icon: Calculator,
    tone: "amber",
    badge: "内置",
  },
  {
    id: "ihub.tool.time",
    label: "时间与时间戳",
    detail: "Unix、ISO 与时区转换",
    icon: Clock3,
    tone: "blue",
    badge: "内置",
  },
  {
    id: "ihub.tool.qrcode",
    label: "二维码",
    detail: "离线生成、识别与导出 PNG",
    icon: QrCode,
    tone: "mint",
    badge: "内置",
  },
  {
    id: "ihub.tool.cloud-drive",
    label: "云盘",
    detail: "受限 WebDAV 连接与目录浏览",
    icon: Cloud,
    tone: "blue",
    badge: "内置",
  },
  {
    id: "ihub.tool.screen-record",
    label: "屏幕录制",
    detail: "导出 WebM",
    icon: Video,
    tone: "blue",
    badge: "内置",
  },
  {
    id: "ihub.tool.batch-rename",
    label: "批量重命名",
    detail: "先预览，再执行",
    icon: Files,
    tone: "slate",
    badge: "内置",
  },
  {
    id: "ihub.tool.create-plugin",
    label: "创建插件项目",
    detail: "TypeScript + Rust worker 模板",
    icon: Code2,
    tone: "violet",
    badge: "内置",
  },
];

/* First launch has no truthful history yet. Keep the opening rhythm useful by
   showing real, host-owned built-ins in the same compact row; the title makes
   clear that these are suggestions, never fabricated recent applications. */
const firstRunQuickItems = builtinPinnedItems.slice(0, 18);

export const defaultMarketplaceItems: readonly SpotlightLauncherItem[] = [
  {
    id: "ihub.open-plugin-center",
    label: "插件中心",
    detail: "发现、导入与管理插件",
    icon: Puzzle,
    tone: "mint",
    badge: "iHub",
  },
  {
    id: "ihub.open-developer-tools",
    label: "开发者工具",
    detail: "前端 + Rust worker 插件模板",
    icon: Code2,
    tone: "violet",
    badge: "内置",
  },
  {
    id: "ihub.tool.clipboard-history",
    label: "剪贴板历史",
    detail: "本机纯文本记录与固定",
    icon: Clipboard,
    tone: "mint",
    badge: "官方",
  },
  {
    id: "ihub.tool.screenshot",
    label: "截图",
    detail: "选择屏幕并导出 PNG",
    icon: Camera,
    tone: "blue",
    badge: "官方",
  },
  {
    id: "ihub.tool.json",
    label: "JSON 工具",
    detail: "离线格式化与校验",
    icon: Braces,
    tone: "amber",
    badge: "官方",
  },
  {
    id: "ihub.tool.markdown",
    label: "Markdown 工作台",
    detail: "离线写作、安全预览与导出",
    icon: BookOpenText,
    tone: "slate",
    badge: "官方",
  },
  {
    id: "ihub.tool.quick-note",
    label: "速记",
    detail: "本机便签与内容搜索",
    icon: NotebookPen,
    tone: "violet",
    badge: "官方",
  },
  {
    id: "ihub.tool.qrcode",
    label: "二维码",
    detail: "离线生成并识别二维码图片",
    icon: QrCode,
    tone: "mint",
    badge: "官方",
  },
  {
    id: "ihub.tool.cloud-drive",
    label: "云盘",
    detail: "WebDAV 安全连接与目录浏览",
    icon: Cloud,
    tone: "blue",
    badge: "官方",
  },
  {
    id: "ihub.tool.calculator",
    label: "计算器",
    detail: "离线计算表达式并复制结果",
    icon: Calculator,
    tone: "amber",
    badge: "官方",
  },
  {
    id: "ihub.tool.screen-record",
    label: "录屏",
    detail: "系统选择器与 WebM 导出",
    icon: Video,
    tone: "blue",
    badge: "官方",
  },
  {
    id: "ihub.tool.batch-rename",
    label: "批量重命名",
    detail: "预览确认后执行",
    icon: Files,
    tone: "slate",
    badge: "官方",
  },
];

function normalizedSearchText(item: SpotlightLauncherItem) {
  return [item.label, item.detail, item.badge, item.id]
    .filter(Boolean)
    .join(" ")
    .toLocaleLowerCase();
}

function matchesQuery(item: SpotlightLauncherItem, query: string) {
  return normalizedSearchText(item).includes(query.trim().toLocaleLowerCase());
}

function activeGridColumnCount() {
  if (typeof window === "undefined") {
    return 9;
  }
  if (window.innerWidth <= 355) {
    return 3;
  }
  if (window.innerWidth <= 690) {
    return 4;
  }
  if (window.innerWidth <= 760) {
    return 7;
  }
  return 9;
}

const spotlightLauncherStyles = `
  /* Apple high-saturation launcher contract. Keep the measured geometry, with
     translucent material layers and explicit system-color semantics. */
  .ihub-spotlight-scrim {
    appearance: none;
    background: transparent;
    border: 0;
    cursor: default;
    inset: 0;
    position: fixed;
    z-index: 54;
  }

  .ihub-spotlight {
    --ihub-apple-surface: #f5f5f7;
    --ihub-apple-material: rgba(255, 255, 255, .76);
    --ihub-apple-border: rgba(60, 60, 67, .18);
    --ihub-apple-text: #1c1c1e;
    --ihub-apple-input: #0a84ff;
    --ihub-apple-input-empty: #5e5ce6;
    --ihub-apple-placeholder: #7c7c80;
    --ihub-apple-title: #1c1c1e;
    --ihub-apple-label: #2c2c2e;
    --ihub-apple-detail: #636366;
    --ihub-apple-action: #5e5ce6;
    --ihub-apple-provider: #007aff;
    --ihub-apple-hover: rgba(94, 92, 230, .10);
    --ihub-apple-selected: rgba(10, 132, 255, .18);
    --ihub-apple-match: #ff375f;
    --ihub-apple-scroll-track: rgba(94, 92, 230, .08);
    --ihub-apple-scroll-thumb: rgba(10, 132, 255, .68);
    --ihub-apple-avatar-glow-near: rgba(255, 55, 95, .72);
    --ihub-apple-avatar-glow-far: rgba(94, 92, 230, .48);
    background:
      radial-gradient(circle at 96% 6%, rgba(255, 55, 95, .18), transparent 28%),
      radial-gradient(circle at 8% 98%, rgba(100, 210, 255, .20), transparent 31%),
      var(--ihub-apple-surface);
    border: 1px solid var(--ihub-apple-border);
    border-radius: 0;
    box-shadow: inset 0 1px rgba(255, 255, 255, .86);
    color: var(--ihub-apple-text);
    color-scheme: light;
    font-family: system-ui, "PingFang SC", "Helvetica Neue", "Microsoft Yahei", sans-serif;
    height: 100dvh;
    inset: 0;
    margin: 0;
    max-height: none;
    max-width: none;
    overflow: hidden;
    position: fixed;
    width: 100dvw;
    z-index: 55;
  }

  .ihub-spotlight *,
  .ihub-spotlight *::before,
  .ihub-spotlight *::after {
    box-sizing: border-box;
  }

  .ihub-spotlight ::selection {
    background: rgba(191, 90, 242, .38);
  }

  .ihub-spotlight__drag-zone {
    align-items: center;
    cursor: grab;
    display: flex;
    height: 10px;
    justify-content: center;
    left: 50%;
    position: absolute;
    top: 0;
    transform: translateX(-50%);
    touch-action: none;
    user-select: none;
    width: min(160px, 24vw);
    z-index: 3;
  }

  .ihub-spotlight__drag-zone.is-armed {
    cursor: grabbing;
  }

  .ihub-spotlight__search-row {
    background: rgba(255, 255, 255, .36);
    backdrop-filter: blur(22px) saturate(170%);
    display: block;
    height: 56px;
    min-height: 56px;
    padding: 0;
    position: relative;
  }

  .ihub-spotlight__brand,
  .ihub-spotlight__search-field > svg,
  .ihub-spotlight__top-button[aria-label="关闭启动器"],
  .ihub-spotlight__footer {
    display: none;
  }

  .ihub-spotlight__search-field {
    display: block;
    height: 56px;
    min-width: 0;
  }

  .ihub-spotlight__search-field input {
    background: transparent;
    border: 0;
    color: var(--ihub-apple-input);
    font-family: inherit;
    font-size: 18px;
    font-weight: 400;
    height: 56px;
    letter-spacing: 0;
    min-width: 0;
    outline: 0;
    padding: 0 56px 0 12px;
    width: 100%;
  }

  .ihub-spotlight__search-field input:placeholder-shown {
    color: var(--ihub-apple-input-empty);
    font-size: 22px;
  }

  .ihub-spotlight__search-field input::placeholder {
    color: var(--ihub-apple-placeholder);
    font-weight: 200;
    opacity: 1;
    user-select: none;
  }

  .ihub-spotlight__search-actions {
    align-items: center;
    display: flex;
    height: 56px;
    justify-content: center;
    position: absolute;
    right: 0;
    top: 0;
    width: 56px;
  }

  .ihub-spotlight__top-button,
  .ihub-spotlight__tile,
  .ihub-spotlight__open-center {
    appearance: none;
    border: 0;
    cursor: pointer;
    font: inherit;
  }

  .ihub-spotlight__top-button {
    align-items: center;
    background: transparent;
    color: var(--ihub-apple-action);
    display: inline-flex;
    height: 56px;
    justify-content: center;
    padding: 0;
    width: 56px;
  }

  .ihub-spotlight__profile-button {
    background: transparent;
    border: 0;
    border-radius: 0;
    box-shadow: none;
    height: 56px;
    overflow: visible;
    padding: 0;
    width: 56px;
  }

  .ihub-spotlight__profile-avatar {
    border: 0;
    border-radius: 18px;
    display: block;
    height: 36px;
    object-fit: cover;
    width: 36px;
  }

  .ihub-spotlight__profile-button:hover,
  .ihub-spotlight__profile-button:focus-visible {
    background: transparent;
    outline: 0;
  }

  .ihub-spotlight__profile-button:hover .ihub-spotlight__profile-avatar,
  .ihub-spotlight__profile-button:focus-visible .ihub-spotlight__profile-avatar {
    box-shadow:
      0 0 4px 1px var(--ihub-apple-avatar-glow-near),
      0 0 8px 2px var(--ihub-apple-avatar-glow-far);
    filter: brightness(1.1);
    transform: scale(.95);
  }

  .ihub-spotlight__content {
    max-height: min(600px, calc(100dvh - 56px));
    overflow-x: hidden;
    overflow-y: auto;
    padding: 0;
    scrollbar-color: var(--ihub-apple-scroll-thumb) var(--ihub-apple-scroll-track);
    scrollbar-width: auto;
  }

  .ihub-spotlight__content::-webkit-scrollbar {
    height: 8px;
    width: 8px;
  }

  .ihub-spotlight__content::-webkit-scrollbar-track,
  .ihub-spotlight__content::-webkit-scrollbar-track-piece {
    background: var(--ihub-apple-scroll-track);
  }

  .ihub-spotlight__content::-webkit-scrollbar-thumb {
    background: var(--ihub-apple-scroll-thumb);
    border: 2px solid var(--ihub-apple-scroll-track);
    border-radius: 4px;
  }

  .ihub-spotlight__content::-webkit-scrollbar-thumb:hover {
    background: #9f9f9f;
  }

  .ihub-spotlight__group {
    margin: 0 0 6px;
  }

  .ihub-spotlight__group + .ihub-spotlight__group {
    margin-top: 0;
  }

  .ihub-spotlight__group-header {
    align-items: center;
    display: flex;
    height: 28px;
    justify-content: space-between;
    margin: 0;
    padding: 0 12px;
  }

  .ihub-spotlight__group-heading {
    align-items: center;
    display: flex;
    gap: 4px;
    min-width: 0;
  }

  .ihub-spotlight__group-title {
    color: var(--ihub-apple-title);
    font-size: 14px;
    font-weight: 700;
    letter-spacing: 0;
    line-height: 28px;
    margin: 0;
  }

  .ihub-spotlight__group-count {
    color: var(--ihub-apple-action);
    font-family: inherit;
    font-size: 13px;
    font-weight: 400;
    letter-spacing: 0;
  }

  .ihub-spotlight__group-action,
  .ihub-spotlight__group-back {
    align-items: center;
    appearance: none;
    background: transparent;
    border: 0;
    border-radius: 4px;
    color: var(--ihub-apple-action);
    cursor: pointer;
    display: inline-flex;
    font: inherit;
    font-size: 13px;
    font-weight: 400;
    letter-spacing: 0;
    min-height: 24px;
    padding: 0 4px;
  }

  .ihub-spotlight__group-action:hover,
  .ihub-spotlight__group-back:hover {
    background: var(--ihub-apple-hover);
    color: var(--ihub-apple-text);
  }

  .ihub-spotlight__group-action:focus-visible,
  .ihub-spotlight__group-back:focus-visible {
    background: var(--ihub-apple-selected);
    color: var(--ihub-apple-text);
    outline: 0;
  }

  .ihub-spotlight__group.is-expanded .ihub-spotlight__grid {
    min-height: 0;
  }

  .ihub-spotlight__grid {
    column-gap: 0;
    display: grid;
    grid-template-columns: repeat(9, 86px);
    padding-left: 12px;
    row-gap: 0;
  }

  .ihub-spotlight__group--recent .ihub-spotlight__grid,
  .ihub-spotlight__group--pinned .ihub-spotlight__grid,
  .ihub-spotlight__group--pinned .ihub-spotlight__empty {
    min-height: 0;
  }

  .ihub-spotlight__tile {
    align-items: center;
    background: transparent;
    border: 0;
    border-radius: 8px;
    box-shadow: none;
    display: inline-flex;
    flex-direction: column;
    height: 86px;
    justify-content: flex-start;
    min-height: 86px;
    padding: 8px 0 0;
    position: relative;
    text-align: center;
    transform: none;
    width: 86px;
  }

  .ihub-spotlight__tile:hover:not(.is-keyboard-selected) {
    background: var(--ihub-apple-hover);
  }

  .ihub-spotlight__tile:focus-visible {
    background: var(--ihub-apple-selected);
    outline: 0;
  }

  .ihub-spotlight__tile.is-keyboard-selected {
    background: var(--ihub-apple-selected);
    border: 0;
    box-shadow: none;
    outline: 0;
    transform: none;
  }

  .ihub-spotlight__tile:disabled {
    cursor: not-allowed;
    opacity: .45;
  }

  .ihub-spotlight__result-row.is-unavailable,
  .ihub-spotlight__tile.is-unavailable {
    filter: saturate(.48);
    opacity: .58;
  }

  .ihub-spotlight__unavailable-marker {
    align-items: center;
    background: rgba(137, 93, 47, .92);
    border: 1px solid rgba(255, 225, 176, .34);
    border-radius: 999px;
    color: #fff0cf;
    display: inline-flex;
    height: 17px;
    justify-content: center;
    pointer-events: none;
    position: absolute;
    right: 7px;
    top: 7px;
    width: 17px;
    z-index: 1;
  }

  .ihub-spotlight__unavailable-marker svg {
    height: 11px;
    width: 11px;
  }

  .ihub-spotlight__tile-icon {
    align-items: center;
    border: 0;
    border-radius: 6px;
    color: #fff;
    display: inline-flex;
    flex: 0 0 auto;
    height: 32px;
    justify-content: center;
    margin-bottom: 8px;
    width: 32px;
  }

  .ihub-spotlight__tile-icon svg {
    height: 17px;
    width: 17px;
  }

  .ihub-spotlight__tile-icon img {
    border-radius: inherit;
    display: block;
    height: 100%;
    object-fit: cover;
    width: 100%;
  }

  .ihub-spotlight__tile-icon--mint {
    background: #30d158;
  }

  .ihub-spotlight__tile-icon--violet {
    background: #bf5af2;
  }

  .ihub-spotlight__tile-icon--amber {
    background: #ff9f0a;
  }

  .ihub-spotlight__tile-icon--blue {
    background: #0a84ff;
  }

  .ihub-spotlight__tile-icon--slate {
    background: #5e5ce6;
  }

  .ihub-spotlight__tile-icon.is-native {
    background: transparent;
    border-radius: 0;
  }

  .ihub-spotlight__tile-icon.is-native img {
    border-radius: 0;
    object-fit: contain;
  }

  .ihub-spotlight__tile-icon.is-loading-native {
    background: transparent;
    box-shadow: none;
  }

  .ihub-spotlight__tile-label {
    color: var(--ihub-apple-label);
    display: -webkit-box;
    font-size: 12px;
    font-weight: 400;
    letter-spacing: 0;
    line-height: 1.3;
    overflow: hidden;
    padding: 0 8px;
    width: 100%;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
  }

  .ihub-spotlight__tile-detail,
  .ihub-spotlight__tile-badge {
    display: none;
  }

  .ihub-spotlight__group--context {
    border-bottom: 0;
    padding-bottom: 0;
  }

  .ihub-spotlight__group--context .ihub-spotlight__grid {
    gap: 0;
    grid-template-columns: repeat(3, minmax(0, 1fr));
    padding: 0 12px;
  }

  .ihub-spotlight__group--context .ihub-spotlight__tile {
    align-items: center;
    background: transparent;
    border: 0;
    border-radius: 8px;
    display: grid;
    gap: 1px 7px;
    grid-template-columns: 28px minmax(0, 1fr);
    grid-template-rows: auto auto;
    height: 48px;
    justify-content: stretch;
    min-height: 48px;
    padding: 5px 7px;
    text-align: left;
    width: 100%;
  }

  .ihub-spotlight__group--context .ihub-spotlight__tile:hover:not(.is-keyboard-selected) {
    background: var(--ihub-apple-hover);
  }

  .ihub-spotlight__group--context .ihub-spotlight__tile:focus-visible {
    background: var(--ihub-apple-selected);
    outline: 0;
  }

  .ihub-spotlight__group--context .ihub-spotlight__tile.is-keyboard-selected {
    background: var(--ihub-apple-selected);
    border: 0;
  }

  .ihub-spotlight__group--context .ihub-spotlight__tile-icon {
    grid-row: 1 / span 2;
    height: 28px;
    margin: 0;
    width: 28px;
  }

  .ihub-spotlight__group--context .ihub-spotlight__tile-icon svg {
    height: 14px;
    width: 14px;
  }

  .ihub-spotlight__group--context .ihub-spotlight__tile-label {
    color: var(--ihub-apple-label);
    font-size: 12px;
    padding: 0;
    text-align: left;
  }

  .ihub-spotlight__group--context .ihub-spotlight__tile-detail {
    color: var(--ihub-apple-detail);
    display: -webkit-box;
    font-size: 10px;
    line-height: 1.2;
    margin: 0;
    overflow: hidden;
    padding: 0;
    text-align: left;
    white-space: normal;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 1;
  }

  .ihub-spotlight__result-list {
    display: grid;
    gap: 0;
    overflow-x: hidden;
    overflow-y: auto;
    scrollbar-color: #8e8e8e transparent;
    scrollbar-width: thin;
  }

  .ihub-spotlight__result-list::-webkit-scrollbar {
    width: 3px;
  }

  .ihub-spotlight__result-list::-webkit-scrollbar-thumb {
    background: #8e8e8e;
    border: 0;
    border-radius: 1px;
  }

  .ihub-spotlight__result-row {
    align-items: center;
    appearance: none;
    background: transparent;
    border: 0;
    border-radius: 0;
    color: var(--ihub-apple-text);
    cursor: pointer;
    display: grid;
    font: inherit;
    gap: 0;
    grid-template-columns: 56px minmax(0, 1fr) auto;
    height: 48px;
    min-height: 48px;
    padding: 0 12px 0 0;
    position: relative;
    text-align: left;
    transform: none;
    width: 100%;
  }

  .ihub-spotlight__result-row:hover:not(.is-keyboard-selected) {
    background: var(--ihub-apple-hover);
  }

  .ihub-spotlight__result-row:focus-visible {
    background: var(--ihub-apple-selected);
    outline: 0;
  }

  .ihub-spotlight__result-row.is-keyboard-selected {
    background: var(--ihub-apple-selected);
    border: 0;
    outline: 0;
    transform: none;
  }

  .ihub-spotlight__result-row:disabled {
    cursor: not-allowed;
    opacity: .45;
  }

  .ihub-spotlight__result-icon {
    align-items: center;
    background: #5e5ce6;
    border-radius: 6px;
    color: #fff;
    display: inline-flex;
    height: 32px;
    justify-content: center;
    justify-self: center;
    width: 32px;
  }

  .ihub-spotlight__result-icon img {
    border-radius: inherit;
    display: block;
    height: 100%;
    object-fit: cover;
    width: 100%;
  }

  .ihub-spotlight__result-icon.is-native {
    background: transparent;
    border-radius: 0;
  }

  .ihub-spotlight__result-icon.is-native img {
    border-radius: 0;
    object-fit: contain;
  }

  .ihub-spotlight__result-icon.is-loading-native {
    background: transparent;
    box-shadow: none;
  }

  .ihub-spotlight__result-copy {
    min-width: 0;
  }

  .ihub-spotlight__result-label {
    color: var(--ihub-apple-label);
    display: block;
    font-size: 16px;
    font-weight: 400;
    letter-spacing: 0;
    line-height: 22px;
  }

  .ihub-spotlight__result-detail {
    color: var(--ihub-apple-detail);
    display: block;
    font-size: 12px;
    line-height: 18px;
    margin-top: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  .ihub-spotlight__result-badge {
    color: var(--ihub-apple-provider);
    font-family: inherit;
    font-size: 14px;
    font-weight: 300;
    line-height: 48px;
    padding-right: 0;
  }

  .ihub-spotlight__empty {
    align-items: center;
    background: transparent;
    border: 0;
    border-radius: 0;
    color: var(--ihub-apple-detail);
    display: flex;
    font-size: 12px;
    gap: 8px;
    min-height: 48px;
    padding: 0 12px;
  }

  .ihub-spotlight__empty svg {
    color: var(--ihub-apple-detail);
    flex: 0 0 auto;
  }

  .ihub-spotlight__result-badge mark,
  .ihub-spotlight__result-label mark,
  .ihub-spotlight__tile-label mark {
    background: transparent;
    color: var(--ihub-apple-match);
  }

  .ihub-spotlight__shortcuts,
  .ihub-spotlight__status,
  .ihub-spotlight__open-center {
    display: none;
  }

  @media (max-width: 760px) {
    .ihub-spotlight__grid {
      grid-template-columns: repeat(7, 86px);
    }

    .ihub-spotlight__group--context .ihub-spotlight__grid {
      grid-template-columns: repeat(2, minmax(0, 1fr));
    }
  }

  @media (max-width: 690px) {
    .ihub-spotlight__grid {
      grid-template-columns: repeat(4, 86px);
    }

    .ihub-spotlight__group--context .ihub-spotlight__grid {
      grid-template-columns: 1fr;
    }
  }

  @media (max-width: 355px) {
    .ihub-spotlight__grid {
      grid-template-columns: repeat(3, 86px);
    }
  }

  @media (prefers-reduced-motion: reduce) {
    .ihub-spotlight__profile-avatar,
    .ihub-spotlight__result-row,
    .ihub-spotlight__tile {
      transition: none;
      transform: none;
    }
  }

`;

export function SpotlightLauncher({
  open,
  focusSignal,
  onClose,
  onActivate,
  query,
  onQueryChange,
  searchResults,
  contextActions = [],
  recentItems = [],
  showRecent = true,
  onToggleRecent,
  spaceActivates = true,
  pinnedItems = builtinPinnedItems,
  pinnedItemIds = [],
  onTogglePinned,
  onUnavailableItem,
  pastedItems = [],
  onClearPastedItems,
  onPasteFiles,
  onPasteImage,
  onPasteNativeImage,
  marketplaceItems = defaultMarketplaceItems,
  statusText = "iHub 只显示已安装插件与本机内置工具。",
  onOpenPluginCenter,
  onOpenSettings,
  onStartWindowDrag,
}: SpotlightLauncherProps) {
  const inputRef = useRef<HTMLInputElement>(null);
  const selectedItemRef = useRef<HTMLButtonElement>(null);
  const expandedGroupBackRef = useRef<HTMLButtonElement>(null);
  const groupActionRefs = useRef<Partial<Record<LauncherExpandableGroupId, HTMLButtonElement | null>>>({});
  const onStartWindowDragRef = useRef(onStartWindowDrag);
  onStartWindowDragRef.current = onStartWindowDrag;
  const prefersReducedMotion = useReducedMotion();
  const [uncontrolledQuery, setUncontrolledQuery] = useState("");
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [expandedGroupId, setExpandedGroupId] = useState<LauncherExpandableGroupId | null>(null);
  // A seeded first result makes Enter convenient, but must never turn a
  // normal word separator into an accidental command execution. Space only
  // becomes an action after the person has explicitly navigated the grid.
  const [spaceActivationArmed, setSpaceActivationArmed] = useState(false);
  const [dragPending, setDragPending] = useState(false);
  const windowDragControllerRef = useRef<LongPressWindowDragController | null>(null);
  const getWindowDragController = () => {
    windowDragControllerRef.current ??= createLongPressWindowDragController({
      onPendingChange: setDragPending,
      onTrigger: () => {
        const startWindowDrag = onStartWindowDragRef.current;
        if (startWindowDrag) {
          try {
            void Promise.resolve(startWindowDrag()).catch(() => undefined);
          } catch {
            // A host drag is an optional native affordance; a platform that
            // declines it must leave the launcher usable and error-free.
          }
        }
      },
    });
    return windowDragControllerRef.current;
  };
  const activeQuery = query ?? uncontrolledQuery;
  const trimmedQuery = activeQuery.trim();
  const pinnedItemIdSet = useMemo(() => new Set(pinnedItemIds), [pinnedItemIds]);

  const groups = useMemo<LauncherGroup[]>(() => {
    const contextGroup: LauncherGroup[] = contextActions.length ? [{
      id: "context",
      label: "针对当前内容",
      emptyLabel: "当前内容没有可用操作。",
      items: contextActions,
    }] : [];

    if (trimmedQuery && searchResults) {
      return [
        ...contextGroup,
        {
          id: "search",
          label: "搜索结果",
          countLabel: `${searchResults.length} 项`,
          emptyLabel: "没有匹配的本机内容或可用命令。",
          items: searchResults,
        },
      ];
    }

    const filter = (items: readonly SpotlightLauncherItem[]) =>
      trimmedQuery ? items.filter((item) => matchesQuery(item, trimmedQuery)) : items;
    const filteredRecentItems = filter(recentItems);
    const filteredPinnedItems = filter(pinnedItems);
    const filteredMarketplaceItems = filter(marketplaceItems);
    const isFirstLaunch = !trimmedQuery && recentItems.length === 0;

    return [
      ...contextGroup,
      ...(pastedItems.length ? [{
        id: "pasted" as const,
        label: "已粘贴文件",
        countLabel: `${pastedItems.length} 项`,
        emptyLabel: "剪贴板中的文件已不可用。",
        items: filter(pastedItems),
      }] : []),
      ...(showRecent ? [{
        id: "recent" as const,
        label: isFirstLaunch ? "常用功能" : "最近使用",
        countLabel: isFirstLaunch ? "内置工具" : `展开 (${filteredRecentItems.length})`,
        expandable: !isFirstLaunch && filteredRecentItems.length > 0,
        emptyLabel: trimmedQuery ? "最近使用中没有匹配的项目。" : "还没有最近使用项目。",
        items: isFirstLaunch
          ? firstRunQuickItems
          : launcherHomePreview("recent", filteredRecentItems, expandedGroupId === "recent"),
        totalCount: isFirstLaunch ? firstRunQuickItems.length : filteredRecentItems.length,
      }] : []),
      {
        id: "pinned" as const,
        label: "已固定",
        countLabel: "全部 >",
        expandable: filteredPinnedItems.length > 0,
        emptyLabel: "还没有固定项目。",
        items: launcherHomePreview("pinned", filteredPinnedItems, expandedGroupId === "pinned"),
        totalCount: filteredPinnedItems.length,
      },
      ...(trimmedQuery ? [{
        id: "marketplace" as const,
        label: "可用工具",
        emptyLabel: "没有可显示的内置工具入口。",
        items: launcherHomePreview("marketplace", filteredMarketplaceItems),
        totalCount: filteredMarketplaceItems.length,
      }] : []),
    ];
  }, [
    contextActions,
    expandedGroupId,
    marketplaceItems,
    pastedItems,
    pinnedItems,
    recentItems,
    searchResults,
    showRecent,
    trimmedQuery,
  ]);

  const expandedGroup = useMemo(
    () => expandedGroupId ? groups.find((group) => group.id === expandedGroupId) ?? null : null,
    [expandedGroupId, groups],
  );
  const visibleGroups = useMemo(
    () => expandedGroup ? [expandedGroup] : groups,
    [expandedGroup, groups],
  );

  const navigableItems = useMemo(
    () => visibleGroups.flatMap((group) => group.items
      .filter((item) => !item.disabled)
      .map((item) => ({
        groupId: group.id,
        item,
        selectionKey: launcherSelectionKey(group.id, item.id),
      }))),
    [visibleGroups],
  );
  // Context actions sit above the normal results visually, but a text search
  // keeps its established Enter/arrow default. A person can still reach the
  // action strip with the same keyboard traversal or by clicking it.
  const preferredInitialItem = useMemo(
    () => navigableItems.find((item) => item.groupId === "search") ?? navigableItems[0],
    [navigableItems],
  );
  const hasSearchResultsGroup = useMemo(
    () => visibleGroups.some((group) => group.id === "search"),
    [visibleGroups],
  );

  useEffect(() => {
    if (!open) {
      setSelectedKey(null);
      setSpaceActivationArmed(false);
      setExpandedGroupId(null);
      return;
    }
    const focusTimer = window.setTimeout(() => inputRef.current?.focus(), 40);
    return () => window.clearTimeout(focusTimer);
  }, [focusSignal, open]);

  useEffect(() => {
    if (!open || !expandedGroupId) {
      return;
    }
    if (!expandedGroup) {
      setExpandedGroupId(null);
      return;
    }
    const frame = window.requestAnimationFrame(() => expandedGroupBackRef.current?.focus());
    return () => window.cancelAnimationFrame(frame);
  }, [expandedGroup, expandedGroupId, open]);

  useEffect(() => () => {
    windowDragControllerRef.current?.dispose();
    windowDragControllerRef.current = null;
  }, []);

  useEffect(() => {
    if (!open || !onStartWindowDrag) {
      windowDragControllerRef.current?.cancel();
    }
  }, [onStartWindowDrag, open]);

  useEffect(() => {
    if (!open) {
      return;
    }
    setSpaceActivationArmed(false);
    setSelectedKey((current) => {
      if (current && navigableItems.some((item) => item.selectionKey === current)) {
        return current;
      }
      return preferredInitialItem?.selectionKey ?? null;
    });
  }, [navigableItems, open, preferredInitialItem]);

  useEffect(() => {
    if (!open || !selectedKey) {
      return;
    }
    // The launcher can be taller than a small display. Keep keyboard focus
    // visible without stealing the text cursor or snapping the whole window.
    selectedItemRef.current?.scrollIntoView({
      behavior: prefersReducedMotion ? "auto" : "smooth",
      block: "nearest",
      inline: "nearest",
    });
  }, [open, prefersReducedMotion, selectedKey]);

  const updateQuery = (nextQuery: string) => {
    // Typing always wins over keyboard selection. This includes the space
    // between ordinary multi-word queries such as "local search".
    setSpaceActivationArmed(false);
    // A search result is a distinct launcher state. Do not leave a focused
    // recent/pinned view behind it, or keyboard navigation would appear to
    // target content that is no longer visible.
    if (nextQuery.trim()) {
      setExpandedGroupId(null);
    }
    if (query === undefined) {
      setUncontrolledQuery(nextQuery);
    }
    onQueryChange?.(nextQuery);
  };

  const handleSearchPaste = (event: React.ClipboardEvent<HTMLInputElement>) => {
    const clipboard = event.clipboardData;
    const clipboardTypes = Array.from(clipboard.types).map((type) => type.toLocaleLowerCase());

    // Explorer/Finder copies are exposed as a native clipboard file list on
    // Windows/macOS. Prefer that exact signal so a copied PNG file opens as
    // a file rather than being mistaken for a bitmap screenshot.
    const hasNativeFilePayload = clipboardTypes.includes("files")
      || (clipboard.files.length > 0
        && !Array.from(clipboard.items).some((item) =>
          item.kind === "file" && item.type.toLocaleLowerCase().startsWith("image/"),
        ));
    if (hasNativeFilePayload && onPasteFiles) {
      event.preventDefault();
      void Promise.resolve(onPasteFiles()).catch(() => undefined);
      return;
    }

    const imageItem = Array.from(clipboard.items).find((item) =>
      item.kind === "file" && item.type.toLocaleLowerCase().startsWith("image/"),
    );
    const image = imageItem?.getAsFile();
    if (image) {
      event.preventDefault();
      onPasteImage?.({
        blob: image,
        name: image.name || "clipboard-image.png",
        type: image.type || "image/png",
      });
      return;
    }

    // WebView2/WKWebView may advertise an image MIME type without exposing a
    // DOM File. Fall back to the native clipboard only for that exact case;
    // normal text/HTML paste still follows the browser's default behavior.
    const hasNativeImagePayload = clipboardTypes.some((type) => type.startsWith("image/"));
    if (hasNativeImagePayload && onPasteNativeImage) {
      event.preventDefault();
      void Promise.resolve(onPasteNativeImage()).catch(() => undefined);
    }
  };

  const handleWindowDragPointerDown = (event: React.PointerEvent<HTMLDivElement>) => {
    if (!onStartWindowDrag || !supportsLongPressWindowDrag(event)) {
      return;
    }
    event.preventDefault();
    const windowDragController = getWindowDragController();
    if (!windowDragController.begin({
      pointerId: event.pointerId,
      x: event.clientX,
      y: event.clientY,
    })) {
      return;
    }
    try {
      event.currentTarget.setPointerCapture(event.pointerId);
    } catch {
      windowDragController.cancel(event.pointerId);
    }
  };

  const handleWindowDragPointerMove = (event: React.PointerEvent<HTMLDivElement>) => {
    windowDragControllerRef.current?.move({
      pointerId: event.pointerId,
      x: event.clientX,
      y: event.clientY,
    });
  };

  const activate = (item: SpotlightLauncherItem) => {
    if (item.disabled || item.unavailable) {
      if (item.unavailable) {
        onUnavailableItem?.(item);
      }
      return;
    }
    if (item.id === "ihub.open-plugin-center" && onOpenPluginCenter) {
      if (onActivate) {
        onActivate(item);
        return;
      }
      onOpenPluginCenter();
      return;
    }
    if (item.id === "ihub.open-settings" && onOpenSettings) {
      if (onActivate) {
        onActivate(item);
        return;
      }
      onOpenSettings();
      return;
    }
    onActivate?.(item);
  };

  const toggleSelectedPinned = () => {
    if (!onTogglePinned) {
      return false;
    }
    const selected = navigableItems.find((candidate) => candidate.selectionKey === selectedKey);
    if (!selected || selected.groupId === "pasted") {
      return false;
    }
    if (selected.groupId === "search" && !selected.item.canPinFromSearch) {
      return false;
    }
    onTogglePinned(selected.item);
    return true;
  };

  const moveSelection = (offset: number) => {
    if (!navigableItems.length) {
      return;
    }
    const currentIndex = selectedKey
      ? navigableItems.findIndex((entry) => entry.selectionKey === selectedKey)
      : offset > 0
        ? -1
        : 0;
    const nextIndex = (currentIndex + offset + navigableItems.length) % navigableItems.length;
    setSelectedKey(navigableItems[nextIndex]?.selectionKey ?? null);
    setSpaceActivationArmed(true);
  };

  const moveGroupSelection = (offset: number) => {
    const selectableGroups = visibleGroups.filter((group) => group.items.some((item) => !item.disabled));
    if (!selectableGroups.length) {
      return;
    }
    const currentGroupIndex = selectedKey
      ? selectableGroups.findIndex((group) => group.items.some((item) =>
        launcherSelectionKey(group.id, item.id) === selectedKey,
      ))
      : offset > 0
        ? -1
        : 0;
    const nextGroupIndex = (currentGroupIndex + offset + selectableGroups.length) % selectableGroups.length;
    const nextGroup = selectableGroups[nextGroupIndex];
    const nextItem = nextGroup?.items.find((item) => !item.disabled);
    setSelectedKey(nextGroup && nextItem ? launcherSelectionKey(nextGroup.id, nextItem.id) : null);
    setSpaceActivationArmed(true);
  };

  const moveGridSelection = (direction: "left" | "right" | "up" | "down") => {
    const selectableGroups = visibleGroups
      .map((group) => ({ ...group, items: group.items.filter((item) => !item.disabled) }))
      .filter((group) => group.items.length > 0);
    if (!selectableGroups.length) {
      return;
    }

    const locatedGroupIndex = selectedKey
      ? selectableGroups.findIndex((group) => group.items.some((item) =>
        launcherSelectionKey(group.id, item.id) === selectedKey,
      ))
      : 0;
    const groupIndex = locatedGroupIndex >= 0 ? locatedGroupIndex : 0;
    const currentGroup = selectableGroups[groupIndex];
    if (!currentGroup) {
      return;
    }
    const locatedItemIndex = selectedKey
      ? currentGroup.items.findIndex((item) => launcherSelectionKey(currentGroup.id, item.id) === selectedKey)
      : 0;
    const itemIndex = locatedItemIndex >= 0 ? locatedItemIndex : 0;
    const select = (nextGroupIndex: number, nextItemIndex: number) => {
      const nextGroup = selectableGroups[nextGroupIndex];
      const nextItem = nextGroup?.items[nextItemIndex];
      setSelectedKey(nextGroup && nextItem ? launcherSelectionKey(nextGroup.id, nextItem.id) : null);
      setSpaceActivationArmed(true);
    };

    if (direction === "left") {
      if (itemIndex > 0) {
        select(groupIndex, itemIndex - 1);
      } else if (groupIndex > 0) {
        const previousGroup = selectableGroups[groupIndex - 1];
        select(groupIndex - 1, previousGroup.items.length - 1);
      }
      return;
    }

    if (direction === "right") {
      if (itemIndex + 1 < currentGroup.items.length) {
        select(groupIndex, itemIndex + 1);
      } else if (groupIndex + 1 < selectableGroups.length) {
        select(groupIndex + 1, 0);
      }
      return;
    }

    const columns = activeGridColumnCount();
    if (direction === "up") {
      if (itemIndex >= columns) {
        select(groupIndex, itemIndex - columns);
      } else if (groupIndex > 0) {
        const previousGroup = selectableGroups[groupIndex - 1];
        select(groupIndex - 1, Math.min(previousGroup.items.length - 1, itemIndex % columns));
      }
      return;
    }

    if (itemIndex + columns < currentGroup.items.length) {
      select(groupIndex, itemIndex + columns);
    } else if (groupIndex + 1 < selectableGroups.length) {
      const nextGroup = selectableGroups[groupIndex + 1];
      select(groupIndex + 1, Math.min(nextGroup.items.length - 1, itemIndex % columns));
    }
  };

  const openExpandedGroup = (groupId: LauncherExpandableGroupId) => {
    setExpandedGroupId(groupId);
    setSelectedKey(null);
    setSpaceActivationArmed(false);
  };

  const returnToLauncherGroups = () => {
    const previousGroupId = expandedGroupId;
    setExpandedGroupId(null);
    setSelectedKey(null);
    setSpaceActivationArmed(false);
    if (previousGroupId) {
      window.requestAnimationFrame(() => groupActionRefs.current[previousGroupId]?.focus());
    }
  };

  const handleLauncherDialogKeyDown = (event: React.KeyboardEvent<HTMLElement>) => {
    if (
      event.defaultPrevented
      || event.nativeEvent.isComposing
      || event.key !== "Escape"
      || !expandedGroupId
    ) {
      return;
    }
    event.preventDefault();
    returnToLauncherGroups();
  };

  const handleSearchKeyDown = (event: React.KeyboardEvent<HTMLInputElement>) => {
    if (event.nativeEvent.isComposing) {
      return;
    }
    if (event.altKey && event.key.toLocaleLowerCase() === "h") {
      event.preventDefault();
      onToggleRecent?.();
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      if (expandedGroupId) {
        returnToLauncherGroups();
        return;
      }
      if (trimmedQuery) {
        updateQuery("");
        setSelectedKey(null);
        setSpaceActivationArmed(false);
        return;
      }
      if (pastedItems.length) {
        event.preventDefault();
        onClearPastedItems?.();
        setSelectedKey(null);
        return;
      }
      onClose();
      return;
    }
    if (event.key === "ArrowDown") {
      event.preventDefault();
      if (event.ctrlKey || event.metaKey) {
        moveGroupSelection(1);
      } else if (hasSearchResultsGroup) {
        moveSelection(1);
      } else {
        moveGridSelection("down");
      }
      return;
    }
    if (event.key === "ArrowUp") {
      event.preventDefault();
      if (event.ctrlKey || event.metaKey) {
        moveGroupSelection(-1);
      } else if (hasSearchResultsGroup) {
        moveSelection(-1);
      } else {
        moveGridSelection("up");
      }
      return;
    }
    if (event.key === "ArrowRight") {
      if (!launcherInputUsesHorizontalGridNavigation(activeQuery)) {
        return;
      }
      event.preventDefault();
      if (hasSearchResultsGroup) {
        moveSelection(1);
      } else {
        moveGridSelection("right");
      }
      return;
    }
    if (event.key === "ArrowLeft") {
      if (!launcherInputUsesHorizontalGridNavigation(activeQuery)) {
        return;
      }
      event.preventDefault();
      if (hasSearchResultsGroup) {
        moveSelection(-1);
      } else {
        moveGridSelection("left");
      }
      return;
    }
    if ((event.ctrlKey || event.metaKey) && event.key === "Tab") {
      event.preventDefault();
      moveGroupSelection(event.shiftKey ? -1 : 1);
      return;
    }
    if (event.key === "ContextMenu" || (event.shiftKey && event.key === "F10")) {
      if (toggleSelectedPinned()) {
        event.preventDefault();
      }
      return;
    }
    if (
      event.key === "Enter"
      || (event.key === " " && spaceActivates && spaceActivationArmed && Boolean(selectedKey))
    ) {
      const item = navigableItems.find((candidate) => candidate.selectionKey === selectedKey)?.item
        ?? preferredInitialItem?.item;
      if (item) {
        event.preventDefault();
        activate(item);
      }
    }
  };

  return (
    <>
      <style>{spotlightLauncherStyles}</style>
      <AnimatePresence>
        {open ? (
          <>
            <motion.button
              aria-label="关闭 iHub 启动器"
              className="ihub-spotlight-scrim"
              initial={prefersReducedMotion ? false : { opacity: 0 }}
              animate={{ opacity: 1 }}
              exit={{ opacity: 0 }}
              transition={prefersReducedMotion ? { duration: 0 } : { duration: 0.12, ease: [0.16, 1, 0.3, 1] }}
              onClick={onClose}
              type="button"
            />
            <motion.section
              aria-label="iHub Spotlight 启动器"
              aria-modal="true"
              className="ihub-spotlight"
              initial={prefersReducedMotion ? false : { opacity: 0, scale: 0.994 }}
              animate={{ opacity: 1, scale: 1 }}
              exit={prefersReducedMotion ? { opacity: 0 } : { opacity: 0, scale: 0.996 }}
              onKeyDown={handleLauncherDialogKeyDown}
              role="dialog"
              transition={prefersReducedMotion ? { duration: 0 } : { duration: 0.15, ease: [0.16, 1, 0.3, 1] }}
            >
              <div
                aria-label={`长按 ${WINDOW_DRAG_LONG_PRESS_MS} 毫秒后拖动窗口`}
                className={`ihub-spotlight__drag-zone${dragPending ? " is-armed" : ""}`}
                data-drag-long-press-ms={WINDOW_DRAG_LONG_PRESS_MS}
                data-window-drag-handle=""
                onLostPointerCapture={(event) => windowDragControllerRef.current?.cancel(event.pointerId)}
                onPointerCancel={(event) => windowDragControllerRef.current?.cancel(event.pointerId)}
                onPointerDown={handleWindowDragPointerDown}
                onPointerMove={handleWindowDragPointerMove}
                onPointerUp={(event) => windowDragControllerRef.current?.cancel(event.pointerId)}
                title={`长按 ${WINDOW_DRAG_LONG_PRESS_MS} 毫秒后拖动窗口`}
              />
              <header className="ihub-spotlight__search-row">
                <span aria-hidden="true" className="ihub-spotlight__brand"><Command size={18} strokeWidth={2.35} /></span>
                <label className="ihub-spotlight__search-field">
                  <Search aria-hidden="true" size={20} strokeWidth={2} />
                  <input
                    aria-label="搜索 iHub"
                    autoComplete="off"
                    onChange={(event) => updateQuery(event.target.value)}
                    onKeyDown={handleSearchKeyDown}
                    onPaste={handleSearchPaste}
                    placeholder="搜索功能 / 粘贴文件、图片"
                    ref={inputRef}
                    spellCheck="false"
                    value={activeQuery}
                  />
                </label>
                <div className="ihub-spotlight__search-actions">
                  {onOpenPluginCenter ? (
                    <button
                      aria-label="打开插件中心"
                      className="ihub-spotlight__top-button ihub-spotlight__profile-button"
                      onClick={onOpenPluginCenter}
                      title="插件中心"
                      type="button"
                    >
                      <img alt="" className="ihub-spotlight__profile-avatar" src="/ihub-avatar.svg" />
                    </button>
                  ) : null}
                  <button
                    aria-label="关闭启动器"
                    className="ihub-spotlight__top-button"
                    onClick={onClose}
                    title="关闭（Esc）"
                    type="button"
                  >
                    <X size={18} />
                  </button>
                </div>
              </header>

              <div className="ihub-spotlight__content">
                {visibleGroups.map((group, groupIndex) => (
                  <section
                    aria-labelledby={`ihub-spotlight-${group.id}`}
                    className={`ihub-spotlight__group ihub-spotlight__group--${group.id}${expandedGroupId === group.id ? " is-expanded" : ""}`}
                    key={group.id}
                  >
                    <div className="ihub-spotlight__group-header">
                      <div className="ihub-spotlight__group-heading">
                        {expandedGroupId === group.id ? (
                          <button
                            aria-label={`返回启动器首页（${group.label}）`}
                            className="ihub-spotlight__group-back"
                            onClick={returnToLauncherGroups}
                            ref={expandedGroupBackRef}
                            type="button"
                          >
                            <ChevronLeft aria-hidden="true" size={17} strokeWidth={2.2} />
                            返回
                          </button>
                        ) : null}
                        <h2 className="ihub-spotlight__group-title" id={`ihub-spotlight-${group.id}`}>
                          <BlurText className="ihub-spotlight__group-title-text" text={group.label} />
                        </h2>
                      </div>
                      {group.items.length && group.countLabel ? (
                        isExpandableLauncherGroup(group) && expandedGroupId !== group.id ? (
                          <button
                            aria-controls={`ihub-spotlight-${group.id}-items`}
                            aria-expanded={false}
                            aria-label={`展开${group.label}，共 ${group.totalCount ?? group.items.length} 项`}
                            className="ihub-spotlight__group-action"
                            onClick={() => openExpandedGroup(group.id)}
                            ref={(node) => {
                              groupActionRefs.current[group.id] = node;
                            }}
                            type="button"
                          >
                            {group.countLabel}
                          </button>
                        ) : (
                          <span className="ihub-spotlight__group-count">
                            {expandedGroupId === group.id
                              ? `全部 (${group.totalCount ?? group.items.length})`
                              : group.countLabel}
                          </span>
                        )
                      ) : null}
                    </div>
                    {group.items.length && group.id === "search" ? (
                      <div className="ihub-spotlight__result-list" id={`ihub-spotlight-${group.id}-items`}>
                        {group.items.map((item, itemIndex) => {
                          const Icon = item.icon ?? AppWindow;
                          const nativeIconPending = !item.iconSrc
                            && item.nativeIconPending === true;
                          const canTogglePinned = Boolean(item.canPinFromSearch && onTogglePinned);
                          const baseLabel = item.detail ? `${item.label}：${item.detail}` : item.label;
                          const pinHint = canTogglePinned
                            ? (item.pinnedShortcutId ? "已固定；右键取消固定" : "右键固定到启动页")
                            : null;
                          const unavailableHint = item.unavailable ? "目标当前不可用" : null;
                          const itemHint = [unavailableHint, pinHint].filter(Boolean).join("；");
                          return (
                            <motion.button
                              aria-disabled={item.disabled || item.unavailable || undefined}
                              aria-label={itemHint ? `${baseLabel}（${itemHint}）` : baseLabel}
                              className={`ihub-spotlight__result-row${selectedKey === launcherSelectionKey(group.id, item.id) ? " is-keyboard-selected" : ""}${item.unavailable ? " is-unavailable" : ""}`}
                              disabled={item.disabled}
                              initial={prefersReducedMotion ? false : { opacity: 0, y: 4 }}
                              animate={{ opacity: 1, y: 0 }}
                              key={item.id}
                              onClick={() => activate(item)}
                              onContextMenu={canTogglePinned
                                ? (event) => {
                                  event.preventDefault();
                                  onTogglePinned?.(item);
                                }
                                : undefined}
                              onFocus={() => setSelectedKey(launcherSelectionKey(group.id, item.id))}
                              ref={selectedKey === launcherSelectionKey(group.id, item.id) ? selectedItemRef : undefined}
                              title={itemHint ? `${item.label} · ${itemHint}` : item.detail}
                              transition={prefersReducedMotion
                                ? { duration: 0 }
                                : { delay: Math.min(.1, itemIndex * .012), duration: .16, ease: [0.16, 1, 0.3, 1] }}
                              type="button"
                            >
                              <span
                                aria-hidden="true"
                                className={`ihub-spotlight__result-icon${item.iconSrc ? " is-native" : ""}${nativeIconPending ? " is-loading-native" : ""}`}
                              >
                                {item.iconSrc
                                  ? <img alt="" src={item.iconSrc} />
                                  : nativeIconPending
                                    ? null
                                    : <Icon size={17} strokeWidth={1.9} />}
                              </span>
                              <span className="ihub-spotlight__result-copy">
                                <span className="ihub-spotlight__result-label">{item.label}</span>
                                {item.detail ? <span className="ihub-spotlight__result-detail">{item.detail}</span> : null}
                              </span>
                              {item.unavailable ? <span aria-hidden="true" className="ihub-spotlight__unavailable-marker"><CircleAlert /></span> : null}
                              {item.badge ? <span className="ihub-spotlight__result-badge">{item.badge}</span> : null}
                            </motion.button>
                          );
                        })}
                      </div>
                    ) : group.items.length ? (
                      <div className="ihub-spotlight__grid" id={`ihub-spotlight-${group.id}-items`}>
                        {group.items.map((item, itemIndex) => {
                          const Icon = item.icon ?? AppWindow;
                          const nativeIconPending = !item.iconSrc
                            && item.nativeIconPending === true;
                          const canTogglePinned = group.id !== "pasted"
                            && group.id !== "context"
                            && (group.id !== "search" || item.canPinFromSearch)
                            && Boolean(onTogglePinned);
                          const isPinned = group.id === "pinned" || pinnedItemIdSet.has(item.id);
                          const baseLabel = item.detail ? `${item.label}：${item.detail}` : item.label;
                          const pinHint = canTogglePinned
                            ? (isPinned ? "已固定；右键取消固定" : "右键固定")
                            : null;
                          const unavailableHint = item.unavailable ? "目标当前不可用" : null;
                          const itemHint = [unavailableHint, pinHint].filter(Boolean).join("；");
                          const itemTitle = [
                            item.detail ? `${item.label} · ${item.detail}` : item.label,
                            itemHint,
                          ].filter(Boolean).join(" · ");
                          return (
                            <motion.button
                              aria-disabled={item.disabled || item.unavailable || undefined}
                              aria-label={itemHint ? `${baseLabel}（${itemHint}）` : baseLabel}
                              className={`ihub-spotlight__tile${selectedKey === launcherSelectionKey(group.id, item.id) ? " is-keyboard-selected" : ""}${item.unavailable ? " is-unavailable" : ""}`}
                              disabled={item.disabled}
                              initial={prefersReducedMotion ? false : { opacity: 0, y: 5 }}
                              animate={{ opacity: 1, y: 0 }}
                              key={item.id}
                              onClick={() => activate(item)}
                              onContextMenu={canTogglePinned
                                ? (event) => {
                                  event.preventDefault();
                                  onTogglePinned?.(item);
                                }
                                : undefined}
                              onFocus={() => setSelectedKey(launcherSelectionKey(group.id, item.id))}
                              ref={selectedKey === launcherSelectionKey(group.id, item.id) ? selectedItemRef : undefined}
                              title={itemTitle}
                              transition={prefersReducedMotion
                                ? { duration: 0 }
                                : { delay: Math.min(.12, (groupIndex * 6 + itemIndex) * .012), duration: .18, ease: [0.16, 1, 0.3, 1] }}
                              type="button"
                            >
                              <span
                                aria-hidden="true"
                                className={`ihub-spotlight__tile-icon ihub-spotlight__tile-icon--${item.tone ?? "slate"}${item.iconSrc ? " is-native" : ""}${nativeIconPending ? " is-loading-native" : ""}`}
                              >
                                {item.iconSrc
                                  ? <img alt="" src={item.iconSrc} />
                                  : nativeIconPending
                                    ? null
                                    : <Icon size={19} strokeWidth={1.85} />}
                              </span>
                              <span className="ihub-spotlight__tile-label">{item.label}</span>
                              {item.detail ? <span className="ihub-spotlight__tile-detail">{item.detail}</span> : null}
                              {item.unavailable ? <span aria-hidden="true" className="ihub-spotlight__unavailable-marker"><CircleAlert /></span> : null}
                              {item.badge ? <span className="ihub-spotlight__tile-badge">{item.badge}</span> : null}
                            </motion.button>
                          );
                        })}
                      </div>
                    ) : (
                      <div className="ihub-spotlight__empty" id={`ihub-spotlight-${group.id}-items`}>
                        <Sparkles aria-hidden="true" size={16} />
                        <span>{group.emptyLabel}</span>
                      </div>
                    )}
                  </section>
                ))}
              </div>

              <footer className="ihub-spotlight__footer">
                <div className="ihub-spotlight__shortcuts" aria-label="键盘快捷键提示">
                  <span><kbd>↑↓</kbd> 选择</span>
                  <span><kbd>Enter</kbd> 打开</span>
                  {onTogglePinned ? <span>右键 / <kbd>⇧F10</kbd> 固定</span> : null}
                  <span><kbd>Esc</kbd> 关闭</span>
                  {statusText ? <span className="ihub-spotlight__status">{statusText}</span> : null}
                </div>
              </footer>
            </motion.section>
          </>
        ) : null}
      </AnimatePresence>
    </>
  );
}
