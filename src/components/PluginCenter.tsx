import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import {
  Binary,
  Braces,
  Camera,
  Check,
  Clipboard,
  ChevronRight,
  ChevronUp,
  Cloud,
  Code2,
  Download,
  EllipsisVertical,
  Files,
  FolderSearch,
  LoaderCircle,
  NotebookPen,
  Palette,
  Power,
  Puzzle,
  QrCode,
  RefreshCw,
  Search,
  Settings2,
  ShieldCheck,
  Sparkles,
  Trash2,
  Video,
  X,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import { command, isDesktop } from "../lib/desktop";
import { displayLocalPath } from "../lib/path-display";
import {
  createLongPressWindowDragController,
  type LongPressWindowDragController,
  supportsLongPressWindowDrag,
  WINDOW_DRAG_LONG_PRESS_MS,
} from "../lib/window-drag-long-press";
import {
  PluginArtwork,
  safePluginArtworkSrc,
} from "./PluginArtwork";
import {
  pluginCatalogItemsForView,
  pluginCatalogViewMode,
} from "../lib/plugin-center-catalog-view";
import {
  buildInstalledRailEntries,
  findCatalogEntry,
  pluginCatalog,
  pluginCatalogCategories,
  preferredPluginAcquisition,
  type BuiltinToolId,
  type PluginCatalogCategory,
  type PluginCatalogEntry,
  type PluginCatalogIcon,
  type PluginCatalogTarget,
} from "../lib/plugin-catalog";
import {
  eligibleLauncherContextCommands,
  launcherContextCategoryLabel,
  type LauncherContextEligibleCommand,
  type LauncherContextHandoffPreview,
} from "../lib/plugin-launcher-context";
import { pluginShortcutStatusSummary } from "../lib/plugin-shortcut-status";
import type {
  OfficialWorkspacePluginProject,
  PluginAutomaticUpdateReport,
  PluginCommandInfo,
  PluginInfo,
  PluginLifecycleUpdate,
  PluginUninstallResult,
  PluginUpdateCheck as HostPluginUpdateCheck,
  PluginUpdateResult,
} from "../lib/types";

type PluginCenterFilter = "all" | "installed" | PluginCatalogCategory;

type PluginUpdateStatus = "available" | "up-to-date" | "error" | "unknown";
type PluginLifecycleAction = "enable" | "disable" | "unlink" | "uninstall";

/** Automatic discovery only starts while the Plugin Center is visible. */
const AUTOMATIC_UPDATE_CHECK_INTERVAL_MS = 30 * 60 * 1000;

/**
 * Reopening the center or receiving a fresh plugins array should not fan out
 * another network request immediately. A failed check gets a shorter retry
 * window, while a changed eligible-plugin set always bypasses this memory.
 */
const AUTOMATIC_UPDATE_SUCCESS_COOLDOWN_MS = 2 * 60 * 1000;
const AUTOMATIC_UPDATE_FAILURE_COOLDOWN_MS = 30 * 1000;

/** Renderer-only state adds failure and pending presentation to the host result. */
interface PluginUpdateDisplay {
  status: PluginUpdateStatus;
  message?: string;
  currentCommit?: string;
  latestCommit?: string;
}

interface AutomaticUpdateCheckMemory {
  inFlight: Promise<void> | null;
  lastFinishedAt: number;
  lastOutcome: "success" | "failure" | null;
  lastTargetKey: string | null;
  rerunWhenComplete: boolean;
}

interface MarketplaceItem {
  entry: PluginCatalogEntry;
  installed?: PluginInfo;
}

export interface PluginCenterProps {
  open: boolean;
  /** A transient, host-owned lookup from a launcher context action. This only
   * filters the existing catalog; it never passes the user's content into a
   * plugin or starts an install. */
  initialSearch?: string | null;
  /** A category-only view of content a person explicitly selected in the
   * launcher. The raw source stays in App state and is never rendered here. */
  launcherContext?: LauncherContextHandoffPreview | null;
  plugins: PluginInfo[];
  /** Canonical target reported by the native host. Browser preview leaves it undefined. */
  hostTarget?: string;
  onClose: () => void;
  onPluginsChanged: (plugins: PluginInfo[]) => void;
  onToast: (message: string) => void;
  /** Lets the host open a plugin's bundled UI, when it has one. */
  onOpenFrontend?: (plugin: PluginInfo) => void;
  /** Connect this to the built-in developer/project-template workspace. */
  onOpenDeveloperTools?: () => void;
  /** Opens the host-owned preferences surface from the center's footer. */
  onOpenSettings?: () => void;
  /** Called only after the contextual confirmation button is clicked. The
   * App owns the in-memory source, iframe readiness, and native issuance. */
  onRequestLauncherContextHandoff?: (
    plugin: PluginInfo,
    command: PluginCommandInfo,
  ) => void | Promise<void>;
  /** Starts a native drag after a deliberate long press in the top handle. */
  onStartWindowDrag?: () => void | Promise<void>;
  /** Connect catalog entries such as JSON or batch rename to the matching built-in tool. */
  onOpenBuiltinTool?: (tool: BuiltinToolId) => void;
  /** Optional host-owned installer. Returning a list refreshes the center immediately. */
  onInstallPlugin?: (
    source: string,
    entry?: PluginCatalogEntry,
  ) => Promise<PluginInfo[] | void>;
}

const iconForCatalog: Record<PluginCatalogIcon, typeof Puzzle> = {
  search: FolderSearch,
  ocr: FolderSearch,
  translate: Sparkles,
  palette: Palette,
  clipboard: Clipboard,
  screenshot: Camera,
  json: Braces,
  video: Video,
  rename: Files,
  code: Code2,
  qrcode: QrCode,
  cloud: Cloud,
  window: Settings2,
  note: NotebookPen,
  converter: Binary,
};

const pluginCenterStyles = `
  .plugin-center__scrim {
    background: rgba(0, 0, 0, .38);
    backdrop-filter: blur(3px);
    border: 0;
    cursor: default;
    inset: 0;
    position: fixed;
    z-index: 29;
  }

  .plugin-center {
    --apple-center-glass: rgba(255, 255, 255, .74);
    --apple-center-card: rgba(255, 255, 255, .86);
    --apple-center-separator: rgba(60, 60, 67, .16);
    --apple-center-blue: #0a84ff;
    --apple-center-indigo: #5e5ce6;
    --apple-center-purple: #bf5af2;
    --apple-center-pink: #ff375f;
    --apple-center-orange: #ff9f0a;
    --apple-center-mint: #30d158;
    background:
      radial-gradient(circle at 96% 4%, rgba(255, 55, 95, .15), transparent 28%),
      radial-gradient(circle at 5% 100%, rgba(100, 210, 255, .18), transparent 35%),
      #f5f5f7;
    border: 1px solid rgba(255, 255, 255, .88);
    border-radius: 14px;
    bottom: 0;
    box-shadow: 0 24px 68px rgba(94, 92, 230, .20), inset 0 1px rgba(255, 255, 255, .96);
    color: #1c1c1e;
    color-scheme: light;
    display: grid;
    font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", "Microsoft YaHei", sans-serif;
    grid-template-rows: auto minmax(0, 1fr);
    left: 0;
    max-height: 100dvh;
    overflow: hidden;
    position: fixed;
    right: 0;
    top: 0;
    z-index: 30;
  }

  .plugin-center__topbar {
    align-items: center;
    background: var(--apple-center-glass);
    backdrop-filter: blur(24px) saturate(170%);
    border-bottom: 1px solid var(--apple-center-separator);
    display: grid;
    gap: 12px;
    grid-template-columns: 220px minmax(127px, 1fr) auto;
    min-height: 48px;
    padding: 0 12px;
    position: relative;
  }

  .plugin-center__drag-zone {
    cursor: grab;
    height: 10px;
    left: 50%;
    position: absolute;
    top: 0;
    touch-action: none;
    transform: translateX(-50%);
    user-select: none;
    width: min(160px, 24vw);
    z-index: 2;
  }

  .plugin-center__drag-zone.is-armed { cursor: grabbing; }

  .plugin-center__crumbs,
  .plugin-center__top-actions,
  .plugin-center__search,
  .plugin-center__installed-item,
  .plugin-center__market-action,
  .plugin-center__developer-link,
  .plugin-center__profile-link,
  .plugin-center__hub-mark {
    align-items: center;
    display: flex;
  }

  .plugin-center__crumbs {
    background: transparent;
    border-radius: 5px;
    color: #515151;
    font-size: 13px;
    font-weight: 400;
    gap: 6px;
    letter-spacing: 0;
    min-height: 30px;
    padding: 0 6px;
    white-space: nowrap;
  }

  .plugin-center__crumb-mark {
    align-items: center;
    background: #212124;
    border-radius: 50%;
    color: #fff;
    display: inline-flex;
    height: 23px;
    justify-content: center;
    width: 23px;
  }

  .plugin-center__crumb-title:first-of-type { padding-right: 2px; }
  .plugin-center__crumb-current {
    color: #212121;
    font-weight: 500;
  }

  .plugin-center__crumb-close {
    align-items: center;
    background: transparent;
    border: 0;
    border-radius: 50%;
    color: #737373;
    cursor: pointer;
    display: inline-flex;
    height: 20px;
    justify-content: center;
    margin-left: auto;
    width: 20px;
  }
  .plugin-center__crumb-close:hover { background: rgba(0, 0, 0, .05); color: #212121; }

  .plugin-center__crumb-separator {
    color: #888;
    font-weight: 500;
  }

  .plugin-center__search {
    background: transparent;
    border: 0;
    border-radius: 0;
    color: #737373;
    gap: 0;
    min-height: 32px;
    padding: 0;
  }

  .plugin-center__search:focus-within {
    box-shadow: none;
  }

  .plugin-center__search input {
    background: transparent;
    border: 0;
    color: #212121;
    font: inherit;
    font-size: 16px;
    font-weight: 400;
    min-width: 0;
    outline: 0;
    width: 100%;
  }

  .plugin-center__search svg { display: none; }
  .plugin-center__search input::placeholder { color: #888; }

  .plugin-center__top-actions { gap: 5px; position: relative; }

  .plugin-center__action-menu-shell { position: relative; }

  .plugin-center__action-menu-trigger,
  .plugin-center__close,
  .plugin-center__import-submit {
    align-items: center;
    border: 0;
    cursor: pointer;
    display: inline-flex;
    justify-content: center;
  }

  .plugin-center__action-menu-trigger {
    background: transparent;
    border: 0;
    border-radius: 5px;
    color: #595959;
    height: 26px;
    padding: 0;
    width: 26px;
  }

  .plugin-center__action-menu-trigger:hover,
  .plugin-center__action-menu-trigger[aria-expanded="true"] { background: rgba(0, 0, 0, .05); color: #212121; }

  .plugin-center__action-menu-trigger:focus-visible,
  .plugin-center__action-menu-item:focus-visible {
    outline: 2px solid rgba(10, 132, 255, .48);
    outline-offset: 2px;
  }

  .plugin-center__action-menu {
    background: #fff;
    border: 1px solid rgba(60, 60, 67, .18);
    border-radius: 5px;
    box-shadow: 0 11px 28px rgba(0, 0, 0, .16);
    display: grid;
    gap: 1px;
    min-width: 145px;
    padding: 3px;
    position: absolute;
    right: 0;
    top: calc(100% + 5px);
    transform-origin: top right;
    z-index: 4;
  }

  .plugin-center__action-menu-item {
    align-items: center;
    background: transparent;
    border: 0;
    border-radius: 4px;
    color: #404040;
    cursor: pointer;
    display: grid;
    font: inherit;
    gap: 6px;
    grid-template-columns: 15px minmax(0, 1fr);
    min-height: 26px;
    padding: 0 6px;
    text-align: left;
    width: 100%;
  }

  .plugin-center__action-menu-item:hover { background: rgba(0, 0, 0, .04); color: #212121; }
  .plugin-center__action-menu-item svg { color: #0a84ff; }
  .plugin-center__action-menu-item span { font-size: 12px; font-weight: 400; letter-spacing: 0; }
  .plugin-center__action-menu-separator { background: #e6e6e6; height: 1px; margin: 2px 3px; }

  .plugin-center__hub-mark {
    background: #212124;
    border: 0;
    border-radius: 50%;
    color: #f1f1f1;
    cursor: pointer;
    height: 32px;
    justify-content: center;
    padding: 0;
    width: 32px;
  }
  .plugin-center__hub-mark:hover { background: #0a84ff; }

  .plugin-center__close {
    background: transparent;
    border-radius: 5px;
    color: #737373;
    height: 26px;
    width: 26px;
  }

  .plugin-center__close:hover { background: rgba(0, 0, 0, .05); color: #212121; }

  .plugin-center__import-popover {
    background: #fff;
    border: 1px solid rgba(60, 60, 67, .18);
    border-radius: 7px;
    box-shadow: 0 12px 31px rgba(0, 0, 0, .16);
    display: grid;
    gap: 6px;
    padding: 8px;
    position: absolute;
    right: 0;
    top: calc(100% + 5px);
    width: min(240px, calc(100vw - 21px));
    z-index: 4;
  }

  .plugin-center__import-popover label {
    color: #212121;
    font-size: 13px;
    font-weight: 500;
  }

  .plugin-center__import-popover p {
    color: #737373;
    font-size: 12px;
    line-height: 1.5;
    margin: -3px 0 0;
  }

  .plugin-center__import-field {
    align-items: center;
    background: #fafafa;
    border: 1px solid rgba(60, 60, 67, .18);
    border-radius: 5px;
    display: flex;
    min-height: 24px;
    overflow: hidden;
  }

  .plugin-center__import-field:focus-within { border-color: #0a84ff; box-shadow: 0 0 0 2px rgba(10, 132, 255, .12); }

  .plugin-center__import-field input {
    background: transparent;
    border: 0;
    color: #212121;
    flex: 1;
    font-family: inherit;
    font-size: 12px;
    min-width: 0;
    outline: 0;
    padding: 0 6px;
  }

  .plugin-center__import-field input::placeholder { color: #a6a6a6; }

  .plugin-center__import-submit {
    background: #0a84ff;
    border-radius: 4px;
    color: #fff;
    height: 18px;
    margin-right: 3px;
    width: 19px;
  }

  .plugin-center__import-submit:disabled { cursor: not-allowed; opacity: .45; }
  .plugin-center__import-submit.is-loading:disabled { cursor: progress; opacity: .7; }

  .plugin-center__import-hint.is-ready { color: #389e0d; }
  .plugin-center__import-hint.is-invalid { color: #cf1322; }

  .plugin-center__body {
    display: grid;
    grid-template-columns: 220px minmax(0, 1fr);
    min-height: 0;
  }

  .plugin-center__sidebar {
    background: rgba(244, 247, 255, .72);
    backdrop-filter: blur(22px) saturate(165%);
    border-right: 1px solid var(--apple-center-separator);
    display: flex;
    flex-direction: column;
    min-height: 0;
    overflow: hidden;
    padding: 0;
  }

  .plugin-center__side-heading {
    align-items: center;
    color: #515151;
    display: flex;
    font-size: 13px;
    font-weight: 400;
    height: 40px;
    justify-content: space-between;
    letter-spacing: 0;
    padding: 0 14px 0 18px;
  }

  .plugin-center__side-heading small {
    color: #888;
    font-family: inherit;
    font-size: 12px;
    font-weight: 400;
    letter-spacing: 0;
  }

  .plugin-center__installed {
    border-top: 0;
    flex: 1 1 auto;
    margin-top: 0;
    min-height: 0;
    overflow: auto;
    padding: 0 8px 8px;
  }

  .plugin-center__installed-empty {
    color: #888;
    font-size: 12px;
    line-height: 1.55;
    margin: 1px 5px;
  }

  .plugin-center__installed-list { display: grid; gap: 0; }

  .plugin-center__installed-item {
    background: transparent;
    border: 0;
    border-radius: 5px;
    color: #404040;
    cursor: pointer;
    gap: 0;
    min-height: 42px;
    padding: 8px 6px 8px 12px;
    text-align: left;
    width: 100%;
  }

  .plugin-center__installed-item:hover { background: rgba(94, 92, 230, .10); }
  .plugin-center__installed-item.is-selected { background: rgba(10, 132, 255, .18); }
  .plugin-center__installed-item.is-disabled { color: #a6a6a6; }
  .plugin-center__installed-item.is-disabled .plugin-center__installed-icon { filter: saturate(.38); opacity: .68; }

  .plugin-center__installed-icon,
  .plugin-center__market-icon {
    align-items: center;
    display: inline-flex;
    flex: 0 0 auto;
    justify-content: center;
  }

  .plugin-center__installed-icon {
    background: #5e5ce6;
    border-radius: 4px;
    color: #f2f2f2;
    height: 26px;
    overflow: hidden;
    width: 26px;
  }

  .plugin-center__installed-icon.is-artwork,
  .plugin-center__market-icon.is-artwork {
    background: transparent;
  }

  .plugin-center__installed-icon img,
  .plugin-center__market-icon img {
    display: block;
    height: 100%;
    object-fit: contain;
    width: 100%;
  }

  .plugin-center__installed-icon--json { background: #ff9f0a; }
  .plugin-center__installed-icon--code { background: #0a84ff; }
  .plugin-center__installed-icon--palette { background: #bf5af2; }
  .plugin-center__installed-icon--qrcode { background: #64d2ff; }
  .plugin-center__installed-icon--cloud { background: #5e5ce6; }
  .plugin-center__installed-icon--video { background: #ff375f; }
  .plugin-center__installed-icon--rename { background: #0a84ff; }
  .plugin-center__installed-icon--search { background: #30d158; }
  .plugin-center__installed-icon--clipboard { background: #30d158; }
  .plugin-center__installed-icon--note { background: #bf5af2; }
  .plugin-center__installed-icon--converter { background: #ff9f0a; }

  .plugin-center__installed-copy { flex: 1; min-width: 0; padding: 0 8px; }
  .plugin-center__installed-copy strong { color: #212121; display: block; font-size: 13px; font-weight: 400; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .plugin-center__installed-copy small { color: #878787; display: block; font-family: inherit; font-size: 11px; line-height: 1.2; margin-top: 1px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }

  .plugin-center__side-footer {
    align-items: center;
    background: #f5f5f7;
    border-top: 1px solid rgba(60, 60, 67, .18);
    display: flex;
    flex: 0 0 auto;
    gap: 5px;
    margin-top: 0;
    padding: 4px 8px 8px;
  }

  .plugin-center__profile-link {
    background: transparent;
    border: 0;
    color: #212121;
    cursor: pointer;
    flex: 1;
    font-size: 14px;
    font-weight: 400;
    gap: 7px;
    min-height: 40px;
    padding: 0 4px;
    text-align: left;
  }

  .plugin-center__profile-link img { border-radius: 50%; height: 28px; width: 28px; }
  .plugin-center__profile-link svg { color: #888; margin-left: auto; }
  .plugin-center__profile-link:hover { background: rgba(0, 0, 0, .05); color: #212121; }

  .plugin-center__developer-link {
    background: #fff;
    border: 0;
    border-radius: 6px;
    color: #595959;
    cursor: pointer;
    height: 27px;
    justify-content: center;
    padding: 0;
    width: 28px;
  }

  .plugin-center__developer-link:hover { background: rgba(0, 0, 0, .05); color: #0a84ff; }

  .plugin-center__main {
    background: rgba(255, 255, 255, .50);
    backdrop-filter: blur(20px) saturate(150%);
    min-height: 0;
    overflow: auto;
    padding: 0;
    scrollbar-color: #bfbfbf #f5f5f7;
    scrollbar-width: thin;
  }

  .plugin-center__page-body {
    box-sizing: border-box;
    margin-left: auto;
    margin-right: auto;
    max-width: 800px;
    padding: 12px 20px 23px;
    width: 100%;
  }

  .plugin-center__installed { scrollbar-color: #bfbfbf #f5f5f7; scrollbar-width: thin; }
  .plugin-center__main::-webkit-scrollbar,
  .plugin-center__installed::-webkit-scrollbar { width: 7px; }
  .plugin-center__main::-webkit-scrollbar-track,
  .plugin-center__installed::-webkit-scrollbar-track { background: #f5f5f7; }
  .plugin-center__main::-webkit-scrollbar-thumb,
  .plugin-center__installed::-webkit-scrollbar-thumb { background: #bfbfbf; border: 2px solid #f5f5f7; border-radius: 8px; }
  .plugin-center__main::-webkit-scrollbar-thumb:hover,
  .plugin-center__installed::-webkit-scrollbar-thumb:hover { background: #888; }

  .plugin-center__market-header { display: none; }
  .plugin-center__market-header h2 { color: #212121; font-size: 14px; letter-spacing: 0; margin: 0; }
  .plugin-center__market-header p { color: #737373; font-size: 12px; margin: 3px 0 0; }
  .plugin-center__market-count { color: #0a84ff; font-family: inherit; font-size: 12px; white-space: nowrap; }

  .plugin-center__catalog-filters {
    align-items: center;
    display: flex;
    gap: 4px;
    margin: 0 0 12px;
    min-width: 0;
    overflow-x: auto;
    padding: 0 1px;
    scrollbar-width: none;
  }

  .plugin-center__catalog-filters::-webkit-scrollbar { display: none; }

  .plugin-center__catalog-filter {
    align-items: center;
    background: transparent;
    border: 0;
    border-radius: 5px;
    color: #595959;
    cursor: pointer;
    display: inline-flex;
    flex: 0 0 auto;
    font: inherit;
    font-size: 12px;
    gap: 4px;
    min-height: 26px;
    padding: 0 8px;
    transition: background-color 120ms ease, border-color 120ms ease, color 120ms ease;
  }

  .plugin-center__catalog-filter small {
    color: #888;
    font-family: inherit;
    font-size: 11px;
  }

  .plugin-center__catalog-filter:hover {
    background: rgba(94, 92, 230, .10);
    color: #212121;
  }

  .plugin-center__catalog-filter.is-active {
    background: rgba(10, 132, 255, .18);
    color: #212121;
  }

  .plugin-center__catalog-filter.is-active small { color: #515151; }
  .plugin-center__catalog-filter:focus-visible { outline: 2px solid rgba(10, 132, 255, .48); outline-offset: 1px; }

  .plugin-center__featured-heading { align-items: center; display: flex; justify-content: space-between; margin-bottom: 12px; }
  .plugin-center__featured-heading h3 { color: #212121; font-size: 14px; font-weight: 600; letter-spacing: 0; margin: 0; }
  .plugin-center__featured-actions { align-items: center; display: flex; gap: 9px; }
  .plugin-center__catalog-toggle {
    align-items: center;
    background: transparent;
    border: 0;
    border-radius: 5px;
    color: #0a84ff;
    cursor: pointer;
    display: inline-flex;
    font: inherit;
    font-size: 12px;
    font-weight: 500;
    gap: 4px;
    min-height: 24px;
    padding: 2px 5px;
  }
  .plugin-center__catalog-toggle:hover { background: rgba(0, 0, 0, .04); color: #5e5ce6; }
  .plugin-center__catalog-toggle:focus-visible { outline: 2px solid rgba(10, 132, 255, .48); outline-offset: 1px; }
  .plugin-center__catalog-toggle-count { color: #0a84ff; font-family: inherit; font-size: 11px; font-weight: 400; }

  .plugin-center__market-grid {
    background: #fff;
    border: 0;
    border-radius: 7px;
    display: grid;
    gap: 20px;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    overflow: hidden;
    padding: 12px;
  }

  .plugin-center__market-item {
    align-items: start;
    background: #fff;
    border-bottom: 1px solid #e6e6e6;
    display: grid;
    gap: 0 15px;
    grid-template-columns: 42px minmax(0, 1fr) auto;
    min-height: 54px;
    padding: 0 0 12px;
  }

  .plugin-center__market-item:nth-child(odd) { border-right: 0; }
  .plugin-center__market-item:nth-last-child(-n + 2) { border-bottom: 0; }
  .plugin-center__market-item:nth-child(1) { border-radius: 0; }
  .plugin-center__market-item:nth-child(2) { border-radius: 0; }
  .plugin-center__market-item:nth-last-child(2) { border-radius: 0; }
  .plugin-center__market-item:last-child { border-radius: 0; }
  .plugin-center__market-item.is-disabled .plugin-center__market-icon { filter: saturate(.38); opacity: .68; }
  .plugin-center__market-item.is-disabled .plugin-center__market-copy small { color: #bba77c; }
  .plugin-center__market-item.is-platform-unsupported .plugin-center__market-icon { filter: saturate(.52); opacity: .76; }
  .plugin-center__market-item.is-platform-unsupported .plugin-center__market-copy small { color: #d0ae79; }

  .plugin-center__market-icon {
    background: #4b555c;
    border-radius: 6px;
    color: #f3f3f3;
    height: 42px;
    overflow: hidden;
    width: 42px;
  }

  .plugin-center__market-icon--json { background: #514f59; }
  .plugin-center__market-icon--code { background: #258ee7; }
  .plugin-center__market-icon--palette { background: #7257c4; }
  .plugin-center__market-icon--qrcode { background: #4b82df; }
  .plugin-center__market-icon--cloud { background: #3c77b6; }
  .plugin-center__market-icon--video { background: #e17b4e; }
  .plugin-center__market-icon--rename { background: #4f76dc; }
  .plugin-center__market-icon--search { background: #286ec4; }
  .plugin-center__market-icon--ocr { background: #547bce; }
  .plugin-center__market-icon--translate { background: #6658c6; }
  .plugin-center__market-icon--clipboard { background: #47a879; }
  .plugin-center__market-icon--note { background: #5e6470; }
  .plugin-center__market-icon--converter { background: #9b7a39; }
  .plugin-center__market-icon--window { background: #526371; }

  .plugin-center__market-copy { min-width: 0; }
  .plugin-center__market-copy strong { color: #212121; display: block; font-size: 14px; font-weight: 400; line-height: 1.25; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .plugin-center__market-copy p { color: #737373; display: -webkit-box; font-size: 12px; line-height: 1.5; margin: 4px 0 0; overflow: hidden; -webkit-box-orient: vertical; -webkit-line-clamp: 1; }
  .plugin-center__market-copy small { color: #737373; display: block; font-family: inherit; font-size: 9px; margin-top: 3px; }

  .plugin-center__market-action {
    background: rgba(0, 0, 0, .08);
    border: 0;
    border-radius: 10px;
    color: #333;
    cursor: pointer;
    font-size: 11px;
    font-weight: 500;
    gap: 2px;
    justify-content: center;
    height: 20px;
    min-width: 42px;
    padding: 0 7px;
    white-space: nowrap;
  }

  .plugin-center__market-action:hover:not(:disabled) { background: rgba(0, 0, 0, .16); color: #212121; }
  .plugin-center__market-action.is-installed { color: #389e0d; cursor: default; }
  .plugin-center__market-action.is-pending { color: #888; cursor: default; }
  .plugin-center__market-action.is-platform-unsupported { color: #d48806; cursor: not-allowed; }
  .plugin-center__market-action:disabled { opacity: 1; }

  .plugin-center__market-actions {
    align-items: center;
    display: flex;
    flex-wrap: wrap;
    gap: 3px;
    justify-content: flex-end;
    max-width: 104px;
    min-width: 0;
  }

  .plugin-center__lifecycle-action {
    align-items: center;
    background: #fff;
    border: 1px solid rgba(60, 60, 67, .18);
    border-radius: 10px;
    color: #515151;
    cursor: pointer;
    display: inline-flex;
    font: inherit;
    font-size: 11px;
    font-weight: 500;
    gap: 3px;
    justify-content: center;
    min-height: 20px;
    padding: 0 5px;
    white-space: nowrap;
  }

  .plugin-center__lifecycle-action:hover:not(:disabled) {
    background: rgba(0, 0, 0, .04);
    border-color: #b7b7b7;
    color: #0a84ff;
  }

  .plugin-center__lifecycle-action.is-enable {
    background: #f6ffed;
    border-color: #b7eb8f;
    color: #389e0d;
  }

  .plugin-center__lifecycle-action.is-link { color: #595959; }
  .plugin-center__lifecycle-action.is-danger { color: #cf1322; }
  .plugin-center__lifecycle-action.is-danger:hover:not(:disabled) { background: #fff1f0; border-color: #ffa39e; color: #a8071a; }
  .plugin-center__lifecycle-action:disabled { cursor: progress; opacity: .68; }

  .plugin-center__update-action {
    align-items: center;
    background: rgba(10, 132, 255, .08);
    border: 1px solid rgba(10, 132, 255, .22);
    border-radius: 10px;
    color: #0a84ff;
    cursor: pointer;
    display: inline-flex;
    font-size: 11px;
    font-weight: 500;
    gap: 3px;
    justify-content: center;
    min-height: 20px;
    padding: 0 5px;
    white-space: nowrap;
  }

  .plugin-center__update-action:hover:not(:disabled) {
    background: rgba(10, 132, 255, .16);
    border-color: rgba(10, 132, 255, .34);
    color: #5e5ce6;
  }

  .plugin-center__update-action.is-available {
    background: #0a84ff;
    border-color: #0a84ff;
    color: #fff;
  }

  .plugin-center__update-action.is-current {
    background: transparent;
    border-color: rgba(60, 60, 67, .18);
    color: #888;
  }

  .plugin-center__update-action:disabled { cursor: progress; opacity: .74; }

  .plugin-center__empty {
    border: 1px dashed rgba(60, 60, 67, .18);
    border-radius: 7px;
    color: #737373;
    font-size: 12px;
    line-height: 1.6;
    padding: 28px 16px;
    text-align: center;
  }

  .plugin-center__empty strong { color: #212121; display: block; font-size: 14px; margin-bottom: 3px; }

  /* Explicit launcher-context mode: one calm decision surface, not a catalog
     disguised as a permission picker. */
  .plugin-center__sidebar.is-context { justify-content: space-between; }
  .plugin-center__context-sidebar-copy { display: grid; gap: 9px; padding: 4px 7px; }
  .plugin-center__context-kicker { color: #0a84ff; font-family: inherit; font-size: 11px; letter-spacing: .08em; margin: 0; text-transform: uppercase; }
  .plugin-center__context-sidebar-copy h3 { color: #212121; font-size: 14px; font-weight: 600; letter-spacing: 0; line-height: 1.1; margin: 0; }
  .plugin-center__context-sidebar-copy p:not(.plugin-center__context-kicker) { color: #595959; font-size: 12px; line-height: 1.6; margin: 0; }
  .plugin-center__context-sidebar-note { border-top: 1px solid rgba(60, 60, 67, .18); color: #737373; font-size: 12px; line-height: 1.55; margin: 10px 7px 0; padding-top: 9px; }
  .plugin-center__context-cancel {
    background: #fff; border: 1px solid rgba(60, 60, 67, .18); border-radius: 5px; color: #404040; cursor: pointer;
    font: inherit; font-size: 12px; margin: 0 7px 3px; min-height: 26px; padding: 0 8px; text-align: left;
  }
  .plugin-center__context-cancel:hover { background: rgba(0, 0, 0, .04); border-color: #b7b7b7; color: #0a84ff; }
  .plugin-center__context-main .plugin-center__page-body { max-width: 553px; padding-top: 23px; }
  .plugin-center__context-heading { border-bottom: 1px solid rgba(60, 60, 67, .18); margin-bottom: 15px; padding: 0 0 13px; }
  .plugin-center__context-heading-top { align-items: flex-start; display: flex; gap: 9px; }
  .plugin-center__context-shield { align-items: center; background: rgba(10, 132, 255, .08); border: 1px solid rgba(10, 132, 255, .22); border-radius: 7px; color: #0a84ff; display: inline-flex; height: 26px; justify-content: center; width: 26px; }
  .plugin-center__context-heading h2 { color: #212121; font-size: 16px; font-weight: 600; letter-spacing: 0; line-height: 1.05; margin: 1px 0 0; }
  .plugin-center__context-heading p { color: #595959; font-size: 12px; line-height: 1.55; margin: 5px 0 0; }
  .plugin-center__context-scope { align-items: center; display: flex; flex-wrap: wrap; gap: 5px; margin: 10px 0 0 35px; }
  .plugin-center__context-scope span { background: rgba(10, 132, 255, .08); border: 1px solid rgba(10, 132, 255, .18); border-radius: 999px; color: #5e5ce6; font-family: inherit; font-size: 11px; padding: 3px 5px; }
  .plugin-center__context-scope small { color: #737373; font-size: 11px; }
  .plugin-center__context-list { display: grid; gap: 6px; }
  .plugin-center__context-command {
    align-items: center; background: #fff; border: 1px solid #e6e6e6; border-radius: 7px;
    display: grid; gap: 9px; grid-template-columns: minmax(0, 1fr) auto; min-height: 52px; padding: 9px 9px 9px 10px;
  }
  .plugin-center__context-command:hover { background: rgba(0, 0, 0, .04); border-color: rgba(60, 60, 67, .18); }
  .plugin-center__context-command-copy { min-width: 0; }
  .plugin-center__context-command-copy strong { color: #212121; display: block; font-size: 13px; font-weight: 600; letter-spacing: 0; }
  .plugin-center__context-command-copy strong span { color: #0a84ff; font-family: inherit; font-size: 12px; font-weight: 400; margin-left: 5px; }
  .plugin-center__context-command-copy p { color: #595959; font-size: 12px; line-height: 1.45; margin: 3px 0 0; }
  .plugin-center__context-command-copy small { color: #888; display: block; font-family: inherit; font-size: 11px; margin-top: 3px; }
  .plugin-center__context-command-select,
  .plugin-center__context-confirm-approve,
  .plugin-center__context-confirm-cancel {
    border-radius: 5px; cursor: pointer; font: inherit; font-size: 12px; font-weight: 500; min-height: 26px; padding: 0 8px;
  }
  .plugin-center__context-command-select,
  .plugin-center__context-confirm-approve { background: #0a84ff; border: 1px solid #0a84ff; color: #fff; }
  .plugin-center__context-command-select:hover,
  .plugin-center__context-confirm-approve:hover { background: #5e5ce6; border-color: #5e5ce6; }
  .plugin-center__context-command-select:focus-visible,
  .plugin-center__context-confirm-approve:focus-visible,
  .plugin-center__context-confirm-cancel:focus-visible,
  .plugin-center__context-cancel:focus-visible { outline: 2px solid rgba(10, 132, 255, .48); outline-offset: 2px; }
  .plugin-center__context-empty { border: 1px dashed rgba(60, 60, 67, .18); border-radius: 7px; color: #737373; font-size: 12px; line-height: 1.65; padding: 20px 13px; }
  .plugin-center__context-empty strong { color: #212121; display: block; font-size: 13px; margin-bottom: 3px; }
  .plugin-center__context-confirm { align-items: center; background: rgba(0, 0, 0, .32); display: flex; inset: 0; justify-content: center; padding: 15px; position: absolute; z-index: 7; }
  .plugin-center__context-confirm-card { background: #fff; border: 1px solid rgba(60, 60, 67, .18); border-radius: 9px; box-shadow: 0 17px 47px rgba(0, 0, 0, .22); max-width: 313px; padding: 15px; width: min(100%, 313px); }
  .plugin-center__context-confirm-eyebrow { color: #0a84ff; font-family: inherit; font-size: 11px; letter-spacing: .08em; text-transform: uppercase; }
  .plugin-center__context-confirm-card h2 { color: #212121; font-size: 14px; letter-spacing: 0; line-height: 1.13; margin: 5px 0 0; }
  .plugin-center__context-confirm-card > p { color: #595959; font-size: 12px; line-height: 1.55; margin: 5px 0 0; }
  .plugin-center__context-confirm-scope { background: rgba(10, 132, 255, .06); border: 1px solid rgba(10, 132, 255, .18); border-radius: 6px; margin-top: 11px; padding: 7px 8px; }
  .plugin-center__context-confirm-scope strong { color: #212121; display: block; font-size: 12px; }
  .plugin-center__context-confirm-scope p { color: #595959; font-size: 12px; line-height: 1.55; margin: 3px 0 0; }
  .plugin-center__context-confirm-warning { color: #737373; font-size: 12px; line-height: 1.55; margin: 9px 0 0; }
  .plugin-center__context-confirm-actions { display: flex; gap: 6px; justify-content: flex-end; margin-top: 13px; }
  .plugin-center__context-confirm-cancel { background: #fff; border: 1px solid rgba(60, 60, 67, .18); color: #404040; }
  .plugin-center__context-confirm-cancel:hover { background: #f0f0f0; }
  .plugin-center__context-confirm-approve:disabled,
  .plugin-center__context-confirm-cancel:disabled { cursor: progress; opacity: .68; }

  @media (max-width: 760px) {
    .plugin-center { border-radius: 7px; bottom: 5px; left: 5px; max-height: calc(100dvh - 10px); right: 5px; top: 5px; }
    .plugin-center__topbar { gap: 5px; grid-template-columns: auto minmax(0, 1fr) auto; min-height: 38px; padding: 0 6px; }
    .plugin-center__crumbs > span:not(.plugin-center__crumb-mark) { display: none; }
    .plugin-center__action-menu-trigger { min-width: 22px; padding: 0; }
    .plugin-center__body { grid-template-columns: 1fr; }
    .plugin-center__sidebar:not(.is-context) { display: none; }
    .plugin-center__installed, .plugin-center__side-footer { display: none; }
    .plugin-center__main { padding: 0; }
    .plugin-center__page-body { padding: 11px 9px 16px; }
    .plugin-center__catalog-filters { margin-bottom: 9px; }
    .plugin-center__sidebar.is-context { display: flex; min-height: 103px; overflow: hidden; padding: 8px 5px; }
    .plugin-center__sidebar.is-context .plugin-center__context-sidebar-copy { gap: 5px; padding: 1px 5px; }
    .plugin-center__sidebar.is-context .plugin-center__context-sidebar-note { display: none; }
    .plugin-center__sidebar.is-context .plugin-center__context-cancel { align-self: flex-end; margin: 5px 5px 0; }
    .plugin-center__context-main .plugin-center__page-body { padding-top: 13px; }
    .plugin-center__context-command { align-items: stretch; grid-template-columns: 1fr; }
    .plugin-center__context-command-select { width: 100%; }
    .plugin-center__context-scope { margin-left: 0; }
    .plugin-center__market-grid { grid-template-columns: 1fr; }
    .plugin-center__market-item:nth-child(odd) { border-right: 0; }
    .plugin-center__market-item:nth-last-child(-n + 2) { border-bottom: 1px solid #e6e6e6; }
    .plugin-center__market-item:last-child { border-bottom: 0; }
  }
`;

interface GitHubImportSource {
  isPlausible: boolean;
  requestedRef?: string;
  hint: string;
}

function inspectGitHubImportSource(value: string): GitHubImportSource {
  const normalized = value.trim();
  if (!normalized) {
    return {
      isPlausible: false,
      hint: "输入 owner/repo@tag、github:owner/repo@tag，或完整 GitHub 仓库链接。",
    };
  }
  const githubUrl = /^https:\/\/github\.com\/[A-Za-z0-9][A-Za-z0-9._-]*\/[A-Za-z0-9][A-Za-z0-9._-]*(?:\.git)?(?:#([^\s#]+))?\/?$/i.exec(normalized);
  if (githubUrl) {
    const requestedRef = githubUrl[1];
    return {
      isPlausible: true,
      requestedRef,
      hint: requestedRef
        ? `将解析 ref “${requestedRef}”，并锁定本次解析出的 commit。`
        : "未指定 ref：本次会解析远端 HEAD 并锁定 commit；建议使用发布 tag 或完整 commit。",
    };
  }
  const shorthand = normalized.replace(/^github:/i, "");
  const githubShorthand = /^[A-Za-z0-9][A-Za-z0-9._-]*\/[A-Za-z0-9][A-Za-z0-9._-]*(?:@([^\s@#]+)|#([^\s#]+))?$/.exec(shorthand);
  if (githubShorthand) {
    const requestedRef = githubShorthand[1] ?? githubShorthand[2];
    return {
      isPlausible: true,
      requestedRef,
      hint: requestedRef
        ? `将解析 ref “${requestedRef}”，并锁定本次解析出的 commit。`
        : "未指定 ref：本次会解析远端 HEAD 并锁定 commit；建议使用发布 tag 或完整 commit。",
    };
  }
  return {
    isPlausible: false,
    hint: "格式不正确：请使用 owner/repo@tag、github:owner/repo@tag，或完整 GitHub 链接（可附 #ref）。",
  };
}

function sourceIsPlausible(value: string) {
  return inspectGitHubImportSource(value).isPlausible;
}

function entryMatches(plugin: PluginInfo, entry: PluginCatalogEntry) {
  return plugin.id === entry.id || entry.aliases?.includes(plugin.id) === true;
}

function catalogForInstalledPlugin(plugin: PluginInfo): PluginCatalogEntry {
  return (
    findCatalogEntry(plugin.id) ?? {
      id: plugin.id,
      name: plugin.name,
      description: plugin.description ?? "从 GitHub 导入的插件。",
      category: "productivity",
      distribution: "bootstrap",
      tags: [plugin.id, "已安装"],
      icon: "note",
    }
  );
}

function entrySearchText(entry: PluginCatalogEntry) {
  return [entry.name, entry.id, entry.description, ...entry.tags].join(" ").toLocaleLowerCase();
}

function statusLabel(
  entry: PluginCatalogEntry,
  installed: PluginInfo | undefined,
  workspaceProject: OfficialWorkspacePluginProject | undefined,
  desktopRuntime: boolean,
  workspaceProjectsLoaded: boolean,
) {
  if (installed) {
    if (installed.localLinkStatus === "stale") {
      return installed.usesManagedSnapshotFallback
        ? "源码链接失效 · 正在使用受管快照"
        : "源码链接失效 · 无可用快照";
    }
    if (installed.enabled === false) {
      return installed.isDevelopmentLink ? "本地链接 · 已停用" : "已安装 · 已停用";
    }
    return installed.isDevelopmentLink ? "本地开发链接" : "已安装 · 已启用";
  }
  if (entry.distribution === "builtin") {
    return "内置";
  }
  if (entry.workspaceProject) {
    if (entry.distribution === "installable") {
      if (desktopRuntime && !workspaceProjectsLoaded) {
        return "官方 · 正在检查本机源码";
      }
      return workspaceProject?.available
        ? "官方 · 当前源码可链接"
        : "官方";
    }
    if (!desktopRuntime) {
      return "随源码提供 · 仅桌面开发版";
    }
    if (!workspaceProject) {
      return "随源码提供 · 正在检查";
    }
    return workspaceProject.available
      ? "随源码提供 · 可链接"
      : "随源码提供 · 本机源码不可用";
  }
  if (entry.distribution === "bootstrap") {
    return "筹备中";
  }
  return "官方";
}

const catalogTargetLabels: Record<PluginCatalogTarget, string> = {
  "windows-x86_64": "Windows x64",
  "windows-aarch64": "Windows ARM64",
  "darwin-x86_64": "macOS Intel",
  "darwin-aarch64": "macOS Apple 芯片",
};

function catalogTargetLabel(target: string) {
  return catalogTargetLabels[target as PluginCatalogTarget] ?? target;
}

/**
 * Applies only to official catalog install buttons. Explicit GitHub imports
 * remain user-directed, and an already-installed plugin stays manageable.
 */
function unsupportedPlatformNotice(entry: PluginCatalogEntry, hostTarget?: string) {
  const supportedTargets = entry.supportedTargets;
  if (
    entry.distribution !== "installable"
    || !hostTarget
    || !supportedTargets?.length
    || supportedTargets.includes(hostTarget as PluginCatalogTarget)
  ) {
    return null;
  }
  const supportedLabel = supportedTargets.map(catalogTargetLabel).join("、");
  return {
    status: `当前设备不支持 · 仅支持 ${supportedLabel}`,
    title: `${entry.name} 仅支持 ${supportedLabel}；当前设备：${catalogTargetLabel(hostTarget)}。`,
  };
}

function lifecycleTitle(plugin: PluginInfo) {
  if (plugin.isDevelopmentLink) {
    if (plugin.localLinkStatus === "stale") {
      const localLinkError = displayLocalPath(
        plugin.localLinkError ?? "本地开发源码已不可用。",
      );
      return plugin.usesManagedSnapshotFallback
        ? `${localLinkError}\n解除链接后会继续使用当前受管快照，且不会删除原源码目录。`
        : `${localLinkError}\n解除链接后即可重新安装；iHub 不会删除原源码目录。`;
    }
    return plugin.enabled === false
      ? "本地开发链接已停用；项目目录仍保留在原位置。"
      : "正在直接读取本地开发项目；解除链接不会删除项目目录。";
  }
  return plugin.enabled === false
    ? "插件仍保留在本机，但前端、命令和搜索提供器不会运行。"
    : "插件已启用，可参与启动器命令与已注册的搜索提供器。";
}

function sourceLockLabel(plugin?: PluginInfo) {
  const lock = plugin?.sourceLock;
  if (!lock) {
    return null;
  }
  return `${lock.requestedRef} · ${lock.resolvedCommit.slice(0, 10)}`;
}

function sourceLockTitle(plugin?: PluginInfo) {
  const lock = plugin?.sourceLock;
  if (!lock) {
    return undefined;
  }
  const integrity = lock.integrity;
  const integrityLine = integrity
    ? `运行文件：${integrity.algorithm.toUpperCase()} 已锁定（${integrity.frontendAssets.length} 个前端资源，${integrity.nativeBinaries.length} 个原生二进制）`
    : "运行文件：旧版本来源锁，尚未记录内容哈希；不会自动检查更新，手动“检查更新”仍可使用；重新导入后可升级校验。";
  return `来源：${lock.source}\n请求 ref：${lock.requestedRef}\n已锁定 commit：${lock.resolvedCommit}\n${integrityLine}`;
}

function toPluginUpdateDisplay(check: HostPluginUpdateCheck): PluginUpdateDisplay {
  return {
    status: check.updateAvailable || check.status === "update-available" ? "available" : "up-to-date",
    message: check.message,
    currentCommit: check.currentCommit,
    latestCommit: check.latestCommit,
  };
}

function isGitInstalledPlugin(plugin: PluginInfo) {
  if (plugin.isDevelopmentLink) {
    return false;
  }
  // Current desktop installs always persist a source lock; retain the source
  // shape fallback for plugins imported by an older iHub release.
  if (plugin.sourceLock) {
    return true;
  }
  const source = plugin.source;
  return Boolean(source && sourceIsPlausible(source));
}

function shortCommit(commit?: string) {
  return commit?.slice(0, 10);
}

/** Mirrors the host's deliberately narrow automatic-discovery policy. */
function hasAutomaticUpdateDiscovery(plugin?: PluginInfo) {
  if (
    !plugin
    || plugin.enabled === false
    || plugin.isDevelopmentLink
    || !plugin.autoUpdate
    || plugin.updateChannel !== "stable"
    || !plugin.sourceLock?.integrity
  ) {
    return false;
  }
  const source = plugin.sourceLock?.source;
  const prefix = "https://github.com/neko233-com/";
  if (!source?.startsWith(prefix)) {
    return false;
  }
  const repository = source.slice(prefix.length).replace(/\.git$/, "");
  return Boolean(repository) && /^[A-Za-z0-9._-]+$/.test(repository);
}

function updateStatusLabel(
  update: PluginUpdateDisplay | undefined,
  isChecking: boolean,
  isUpdating: boolean,
) {
  if (isUpdating) {
    return "正在应用更新";
  }
  if (isChecking) {
    return "正在检查更新";
  }
  if (!update) {
    return null;
  }
  if (update.status === "available") {
    return update.latestCommit ? `可更新：${shortCommit(update.latestCommit)}` : "发现可用更新";
  }
  if (update.status === "up-to-date") {
    return "已是最新";
  }
  if (update.status === "error") {
    return "检查失败";
  }
  return update.message ?? "检查结果未知";
}

function updateActionLabel(update: PluginUpdateDisplay | undefined, isChecking: boolean, isUpdating: boolean) {
  if (isUpdating) {
    return "更新中";
  }
  if (isChecking) {
    return "检查中";
  }
  if (update?.status === "available") {
    return "应用更新";
  }
  if (update?.status === "up-to-date") {
    return "再次检查";
  }
  if (update?.status === "error") {
    return "重试检查";
  }
  return "检查更新";
}

function updateActionTitle(update?: PluginUpdateDisplay) {
  if (!update) {
    return undefined;
  }
  const lines = [update.message];
  if (update.currentCommit || update.latestCommit) {
    lines.push(`commit：${shortCommit(update.currentCommit) ?? "未知"} → ${shortCommit(update.latestCommit) ?? "未知"}`);
  }
  if (update.status === "available") {
    lines.push("点击“应用更新”后仍需确认。宿主会重新解析该 ref；若 ref 已移动，或候选改变了权限、原生二进制或命令声明，替换会被拒绝并保留当前版本。");
  }
  return lines.filter(Boolean).join("\n") || undefined;
}

export function PluginCenter({
  open,
  initialSearch,
  launcherContext,
  plugins,
  hostTarget,
  onClose,
  onPluginsChanged,
  onToast,
  onOpenFrontend,
  onOpenDeveloperTools,
  onOpenSettings,
  onRequestLauncherContextHandoff,
  onStartWindowDrag,
  onOpenBuiltinTool,
  onInstallPlugin,
}: PluginCenterProps) {
  const [filter, setFilter] = useState<PluginCenterFilter>("all");
  const [query, setQuery] = useState("");
  const [isActionMenuOpen, setIsActionMenuOpen] = useState(false);
  const [isImportOpen, setIsImportOpen] = useState(false);
  const [importSource, setImportSource] = useState("");
  const [installingSource, setInstallingSource] = useState<string | null>(null);
  const [officialWorkspaceProjects, setOfficialWorkspaceProjects] = useState<
    Record<string, OfficialWorkspacePluginProject>
  >({});
  const [workspaceProjectsLoaded, setWorkspaceProjectsLoaded] = useState(false);
  const [workspaceProbeError, setWorkspaceProbeError] = useState<string | null>(null);
  const [linkingWorkspacePluginId, setLinkingWorkspacePluginId] = useState<string | null>(null);
  const importSourceAnalysis = useMemo(
    () => inspectGitHubImportSource(importSource),
    [importSource],
  );
  const [isAllCatalogExpanded, setIsAllCatalogExpanded] = useState(false);
  const [selectedInstalledId, setSelectedInstalledId] = useState<string | null>(null);
  const [updateChecks, setUpdateChecks] = useState<Record<string, PluginUpdateDisplay>>({});
  const [checkingPluginId, setCheckingPluginId] = useState<string | null>(null);
  const [updatingPluginId, setUpdatingPluginId] = useState<string | null>(null);
  const [pendingLauncherContextTarget, setPendingLauncherContextTarget] = useState<LauncherContextEligibleCommand | null>(null);
  const [requestingLauncherContext, setRequestingLauncherContext] = useState(false);
  const [lifecycleAction, setLifecycleAction] = useState<{
    pluginId: string;
    kind: PluginLifecycleAction;
  } | null>(null);
  const [dragPending, setDragPending] = useState(false);
  const onStartWindowDragRef = useRef(onStartWindowDrag);
  onStartWindowDragRef.current = onStartWindowDrag;
  const windowDragControllerRef = useRef<LongPressWindowDragController | null>(null);
  const actionMenuShellRef = useRef<HTMLDivElement>(null);
  const actionMenuTriggerRef = useRef<HTMLButtonElement>(null);
  const firstActionMenuItemRef = useRef<HTMLButtonElement>(null);
  const shouldReduceMotion = useReducedMotion();
  const desktopRuntime = isDesktop();
  const latestToast = useRef(onToast);
  const latestPlugins = useRef(plugins);
  const automaticUpdateCheckMemory = useRef<AutomaticUpdateCheckMemory>({
    inFlight: null,
    lastFinishedAt: 0,
    lastOutcome: null,
    lastTargetKey: null,
    rerunWhenComplete: false,
  });
  const automaticUpdateRunner = useRef<(() => void) | null>(null);
  const automaticUpdateTargetKey = useMemo(
    () => plugins
      // Disabled plugins are deliberately omitted before issuing the
      // host-wide discovery command. The host also skips them, but avoiding
      // the call altogether when none remain makes the renderer intent clear.
      .filter((plugin) => plugin.enabled !== false && hasAutomaticUpdateDiscovery(plugin))
      .map((plugin) => [
        plugin.id,
        plugin.sourceLock?.source ?? plugin.source ?? "",
        plugin.sourceLock?.resolvedCommit ?? plugin.commit ?? "",
        plugin.sourceLock?.integrity?.manifestSha256 ?? "",
        plugin.updateChannel ?? "",
      ].join(":"))
      .sort()
      .join("|"),
    [plugins],
  );

  const getWindowDragController = () => {
    windowDragControllerRef.current ??= createLongPressWindowDragController({
      onPendingChange: setDragPending,
      onTrigger: () => {
        const startWindowDrag = onStartWindowDragRef.current;
        if (!startWindowDrag) {
          return;
        }
        try {
          void Promise.resolve(startWindowDrag()).catch(() => undefined);
        } catch {
          // Native window dragging is optional; a declined host request must
          // leave the center usable.
        }
      },
    });
    return windowDragControllerRef.current;
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

  useEffect(() => {
    latestToast.current = onToast;
  }, [onToast]);

  useEffect(() => {
    latestPlugins.current = plugins;
  }, [plugins]);

  useEffect(() => {
    if (!open) {
      return;
    }
    if (!desktopRuntime) {
      setOfficialWorkspaceProjects({});
      setWorkspaceProbeError(null);
      setWorkspaceProjectsLoaded(true);
      return;
    }

    let cancelled = false;
    setWorkspaceProjectsLoaded(false);
    setWorkspaceProbeError(null);
    void command<OfficialWorkspacePluginProject[]>("list_official_workspace_plugins")
      .then((projects) => {
        if (cancelled) {
          return;
        }
        setOfficialWorkspaceProjects(
          Object.fromEntries(projects.map((project) => [project.id, project])),
        );
        setWorkspaceProjectsLoaded(true);
      })
      .catch((error: unknown) => {
        if (cancelled) {
          return;
        }
        setOfficialWorkspaceProjects({});
        setWorkspaceProbeError(
          error instanceof Error ? error.message : "无法检查当前源码工作区。",
        );
        setWorkspaceProjectsLoaded(true);
      });

    return () => {
      cancelled = true;
    };
  }, [desktopRuntime, open]);

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
    if (open) {
      return;
    }
    setFilter("all");
    setQuery("");
    setIsActionMenuOpen(false);
    setIsImportOpen(false);
    setImportSource("");
    setSelectedInstalledId(null);
    setIsAllCatalogExpanded(false);
    setLifecycleAction(null);
    setPendingLauncherContextTarget(null);
    setRequestingLauncherContext(false);
  }, [open]);

  useEffect(() => {
    if (launcherContext && open) {
      // Context mode deliberately begins as a candidate picker, not as a
      // catalog search that could make unrelated plugin actions look eligible.
      setFilter("all");
      setQuery("");
      setIsAllCatalogExpanded(false);
      setSelectedInstalledId(null);
      setPendingLauncherContextTarget(null);
      setRequestingLauncherContext(false);
      return;
    }
    const lookup = initialSearch?.trim();
    if (!open || !lookup) {
      return;
    }
    setFilter("all");
    setQuery(lookup);
    setIsAllCatalogExpanded(false);
    setSelectedInstalledId(null);
  }, [initialSearch, launcherContext, open]);

  useEffect(() => {
    if (!open || (!isActionMenuOpen && !isImportOpen)) {
      return;
    }

    const closeTransientActions = () => {
      setIsActionMenuOpen(false);
      setIsImportOpen(false);
    };
    const handlePointerDown = (event: PointerEvent) => {
      const target = event.target;
      if (target instanceof Node && actionMenuShellRef.current?.contains(target)) {
        return;
      }
      closeTransientActions();
    };
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") {
        return;
      }
      event.preventDefault();
      closeTransientActions();
      window.requestAnimationFrame(() => actionMenuTriggerRef.current?.focus());
    };

    window.addEventListener("pointerdown", handlePointerDown);
    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("pointerdown", handlePointerDown);
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [isActionMenuOpen, isImportOpen, open]);

  useEffect(() => {
    if (!open || !isActionMenuOpen) {
      return;
    }
    const frame = window.requestAnimationFrame(() => firstActionMenuItemRef.current?.focus());
    return () => window.cancelAnimationFrame(frame);
  }, [isActionMenuOpen, open]);

  useEffect(() => {
    if (!open || launcherContext || !isDesktop() || !automaticUpdateTargetKey) {
      return;
    }

    let disposed = false;
    const refreshAutomatically = () => {
      const memory = automaticUpdateCheckMemory.current;
      if (memory.inFlight) {
        // If the source/commit set changed while a request is running, make
        // one follow-up pass after it settles. Interval ticks merely coalesce.
        memory.rerunWhenComplete = true;
        return;
      }

      const now = Date.now();
      const targetsChanged = memory.lastTargetKey !== automaticUpdateTargetKey;
      const cooldownMs = memory.lastOutcome === "failure"
        ? AUTOMATIC_UPDATE_FAILURE_COOLDOWN_MS
        : AUTOMATIC_UPDATE_SUCCESS_COOLDOWN_MS;
      if (!targetsChanged && memory.lastFinishedAt && now - memory.lastFinishedAt < cooldownMs) {
        return;
      }

      const requestedTargetKey = automaticUpdateTargetKey;
      let request!: Promise<void>;
      // Begin in a microtask so `inFlight` is assigned even when the desktop
      // bridge rejects synchronously.
      request = Promise.resolve().then(async () => {
        try {
          const report = await command<PluginAutomaticUpdateReport>("check_automatic_plugin_updates");
          memory.lastFinishedAt = Date.now();
          memory.lastOutcome = "success";
          memory.lastTargetKey = requestedTargetKey;
          if (disposed) {
            return;
          }

          // The Rust side rejects disabled plugins too. Filter the response
          // against the renderer's current eligible set so a stale request
          // cannot paint automatic-update data onto a newly disabled plugin.
          const eligiblePlugins = latestPlugins.current.filter(
            (plugin) => plugin.enabled !== false && hasAutomaticUpdateDiscovery(plugin),
          );
          const eligibleIds = new Set(eligiblePlugins.map((plugin) => plugin.id));
          const checks = report.checks.filter((check) => eligibleIds.has(check.pluginId));
          const discovered = Object.fromEntries(
            checks.map((check) => [check.pluginId, toPluginUpdateDisplay(check)]),
          );
          // The host returns skipped entries instead of silently treating a
          // failed integrity/provenance verification as "up to date". Keep
          // that reason on the affected card, while leaving the user-directed
          // Check update action available for a deliberate follow-up.
          const skipped = Object.fromEntries(
            report.skipped
              .filter((skip) => eligibleIds.has(skip.pluginId))
              .map((skip) => [skip.pluginId, {
                status: "error" as const,
                message: skip.reason,
              }]),
          );
          if (Object.keys(discovered).length || Object.keys(skipped).length) {
            setUpdateChecks((current) => ({ ...current, ...discovered, ...skipped }));
          }

          const available = checks.filter((check) => check.updateAvailable || check.status === "update-available");
          if (available.length) {
            const names = available
              .map((check) => eligiblePlugins.find((plugin) => plugin.id === check.pluginId)?.name ?? check.pluginId)
              .slice(0, 3)
              .join("、");
            const remainder = available.length > 3 ? ` 等 ${available.length} 个插件` : "";
            latestToast.current(`插件中心已检查可信稳定插件：${names}${remainder} 有可用更新；尚未下载或应用。`);
          }
        } catch (error) {
          memory.lastFinishedAt = Date.now();
          memory.lastOutcome = "failure";
          memory.lastTargetKey = requestedTargetKey;
          // Discovery is best-effort and should not turn a transient Git or
          // network error into a noisy launcher error. The manual check stays
          // available and intentionally bypasses this automatic cooldown.
          if (!disposed) {
            console.warn("iHub automatic plugin update check failed", error);
          }
        } finally {
          if (memory.inFlight === request) {
            memory.inFlight = null;
          }
          if (memory.rerunWhenComplete) {
            memory.rerunWhenComplete = false;
            automaticUpdateRunner.current?.();
          }
        }
      });
      memory.inFlight = request;
    };

    automaticUpdateRunner.current = refreshAutomatically;
    refreshAutomatically();
    const timer = window.setInterval(refreshAutomatically, AUTOMATIC_UPDATE_CHECK_INTERVAL_MS);
    return () => {
      disposed = true;
      window.clearInterval(timer);
      if (automaticUpdateRunner.current === refreshAutomatically) {
        automaticUpdateRunner.current = null;
        automaticUpdateCheckMemory.current.rerunWhenComplete = false;
      }
    };
  }, [automaticUpdateTargetKey, launcherContext, open]);

  const matchingPlugins = useMemo(
    () => new Map(pluginCatalog.map((entry) => [entry.id, plugins.find((plugin) => entryMatches(plugin, entry))])),
    [plugins],
  );

  const installedEntries = useMemo<MarketplaceItem[]>(
    () =>
      plugins.map((plugin) => ({
        entry: catalogForInstalledPlugin(plugin),
        installed: plugin,
      })),
    [plugins],
  );

  const launcherContextCandidates = useMemo(() => {
    if (!launcherContext) {
      return [];
    }
    const normalizedQuery = query.trim().toLocaleLowerCase();
    return eligibleLauncherContextCommands(plugins, launcherContext)
      .filter(({ plugin, command }) => !normalizedQuery || [
        plugin.name,
        plugin.description,
        command.id,
        command.name,
        command.description,
      ]
        .filter(Boolean)
        .join(" ")
        .toLocaleLowerCase()
        .includes(normalizedQuery));
  }, [launcherContext, plugins, query]);

  // The center mirrors a desktop launcher's "installed apps" rail. Built-ins
  // are genuinely available on this device, so surface them beside imported
  // plugins instead of leaving the rail empty on a first launch.
  const sidebarEntries = useMemo<MarketplaceItem[]>(
    () => buildInstalledRailEntries(plugins),
    [plugins],
  );

  const categoryCounts = useMemo(() => {
    const counts = new Map<PluginCatalogCategory, number>();
    for (const category of pluginCatalogCategories) {
      counts.set(
        category.id,
        pluginCatalog.filter((entry) => entry.category === category.id).length,
      );
    }
    return counts;
  }, []);

  const visibleItems = useMemo<MarketplaceItem[]>(() => {
    const normalizedQuery = query.trim().toLocaleLowerCase();
    const candidates =
      filter === "installed"
        ? installedEntries
        : pluginCatalog
            .filter((entry) => filter === "all" || entry.category === filter)
            .map((entry) => ({ entry, installed: matchingPlugins.get(entry.id) }));

    return candidates
      .filter(({ entry }) => !normalizedQuery || entrySearchText(entry).includes(normalizedQuery))
      .sort((left, right) => Number(Boolean(right.installed)) - Number(Boolean(left.installed)));
  }, [filter, installedEntries, matchingPlugins, query]);

  const catalogViewMode = pluginCatalogViewMode({
    expanded: isAllCatalogExpanded,
    filter,
    query,
  });
  const isDefaultMarketplace = filter === "all" && !query.trim();
  const isCatalogPreview = catalogViewMode === "preview";
  const displayedItems = useMemo<MarketplaceItem[]>(() => {
    const selectedItems = filter === "installed" && selectedInstalledId
      ? visibleItems.filter((item) => item.installed?.id === selectedInstalledId)
      : visibleItems;
    return pluginCatalogItemsForView(selectedItems, catalogViewMode);
  }, [catalogViewMode, filter, selectedInstalledId, visibleItems]);

  const handleBuiltin = (entry: PluginCatalogEntry) => {
    if (!entry.builtinTool) {
      return;
    }
    if (entry.builtinTool === "developer" && onOpenDeveloperTools) {
      onOpenDeveloperTools();
      return;
    }
    if (onOpenBuiltinTool) {
      onOpenBuiltinTool(entry.builtinTool);
      return;
    }
    onToast("该工具已内置，可从主命令框中直接搜索打开。");
  };

  const openGitHubImport = () => {
    setIsActionMenuOpen(false);
    setIsImportOpen(true);
  };

  const openPluginProjectCreator = () => {
    setIsActionMenuOpen(false);
    const developerEntry = pluginCatalog.find((entry) => entry.builtinTool === "developer");
    if (developerEntry) {
      handleBuiltin(developerEntry);
      return;
    }
    onToast("插件项目创建工具暂不可用；可从主命令框搜索“开发者工具”。");
  };

  const openPluginCenterSettings = () => {
    setIsActionMenuOpen(false);
    if (onOpenSettings) {
      onOpenSettings();
      return;
    }
    onToast("偏好设置暂不可用。");
  };

  const installFromSource = async (source: string, entry?: PluginCatalogEntry) => {
    const normalized = source.trim();
    const sourceAnalysis = inspectGitHubImportSource(normalized);
    if (!sourceAnalysis.isPlausible) {
      onToast(sourceAnalysis.hint);
      return;
    }
    if (!isDesktop()) {
      onToast("浏览器预览会展示插件中心，但不会下载或执行第三方插件。");
      return;
    }

    const refSummary = sourceAnalysis.requestedRef
      ? `请求 ref：${sourceAnalysis.requestedRef}\n安装会锁定本次解析出的 commit。`
      : "未指定 ref：本次会解析远端 HEAD，并锁定本次解析出的 commit。建议生产发布使用 tag 或完整 commit。";
    const approved = window.confirm(
      `iHub 将从 GitHub 下载并安装：\n\n${normalized}\n\n${refSummary}\n\n导入不会运行 pnpm/npm、构建脚本或插件 worker。插件前端和原生二进制不在沙箱中运行；请只导入你信任的发布者，并在继续前审阅源码、发行物和权限声明。`,
    );
    if (!approved) {
      onToast("已取消 GitHub 插件导入。");
      return;
    }

    setInstallingSource(normalized);
    try {
      const nextPlugins = onInstallPlugin
        ? await onInstallPlugin(normalized, entry)
        : await (async () => {
            await command<PluginInfo>("install_plugin_from_git", { source: normalized });
            return command<PluginInfo[]>("list_plugins");
          })();
      if (nextPlugins) {
        onPluginsChanged(nextPlugins);
      }
      setImportSource("");
      setIsImportOpen(false);
      onToast("插件已安装并锁定到解析出的 commit。含原生 worker 的命令会在首次启动前再次要求确认。");
    } catch (error) {
      onToast(error instanceof Error ? error.message : "插件安装失败。");
    } finally {
      setInstallingSource(null);
    }
  };

  const linkOfficialWorkspacePlugin = async (entry: PluginCatalogEntry) => {
    if (!entry.workspaceProject) {
      return;
    }
    if (!desktopRuntime) {
      onToast("源码开发插件只能从 Windows 或 macOS 桌面开发版链接。");
      return;
    }
    const project = officialWorkspaceProjects[entry.id];
    if (workspaceProjectsLoaded && !project?.available) {
      onToast(project?.detail ?? workspaceProbeError ?? "当前安装没有可用的 iHub 源码工作区。");
      return;
    }

    setLinkingWorkspacePluginId(entry.id);
    try {
      const plugin = await command<PluginInfo>("link_official_workspace_plugin", {
        pluginId: entry.id,
      });
      const nextPlugins = await command<PluginInfo[]>("list_plugins");
      onPluginsChanged(nextPlugins);
      onToast(`已从当前 iHub checkout 链接 ${plugin.name}；它是本地开发来源，不是市场安装包。`);
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法链接官方源码插件。");
    } finally {
      setLinkingWorkspacePluginId((current) => current === entry.id ? null : current);
    }
  };

  const checkPluginUpdate = async (plugin: PluginInfo) => {
    if (!isDesktop()) {
      onToast("浏览器预览不会连接 GitHub 检查插件更新。");
      return;
    }

    setCheckingPluginId(plugin.id);
    try {
      const response = await command<HostPluginUpdateCheck>("check_plugin_update", { pluginId: plugin.id });
      const update = toPluginUpdateDisplay(response);
      setUpdateChecks((current) => ({ ...current, [plugin.id]: update }));

      if (update.status === "available") {
        onToast(`${plugin.name} 有可用更新。请审阅变更后点击“应用更新”。`);
      } else if (update.status === "up-to-date") {
        onToast(`${plugin.name} 已是最新版本。`);
      }
    } catch (error) {
      const message = error instanceof Error ? error.message : "更新检查失败。";
      setUpdateChecks((current) => ({
        ...current,
        [plugin.id]: { status: "error", message },
      }));
      onToast(message);
    } finally {
      setCheckingPluginId((current) => (current === plugin.id ? null : current));
    }
  };

  const applyPluginUpdate = async (plugin: PluginInfo) => {
    const update = updateChecks[plugin.id];
    if (update?.status !== "available" || !update.latestCommit) {
      void checkPluginUpdate(plugin);
      return;
    }
    if (!isDesktop()) {
      onToast("浏览器预览不会下载或应用插件更新。");
      return;
    }

    const revision = [
      update.currentCommit || update.latestCommit
        ? `${shortCommit(update.currentCommit) ?? "未知"} → ${shortCommit(update.latestCommit) ?? "未知"}`
        : null,
    ].filter(Boolean).join("\n");
    const approved = window.confirm(
      `iHub 将从 ${plugin.sourceLock?.source ?? plugin.source ?? "原 Git 源"} 拉取 ${plugin.name} 的更新。\n\n${revision || "远程仓库有新的已解析 commit。"}\n\n应用前会再次解析保存的 ref，且必须仍指向上面的已审阅 commit。若候选改变了权限、原生二进制或命令声明，常规更新会被拒绝，当前版本不会替换；要接受这类变化，必须卸载后通过 GitHub 导入重新走信任确认。\n\n下载与暂存验证不会启动插件代码或 worker。更新后的插件前端和原生 worker 不在沙箱中运行，请只继续更新你信任的发布者。`,
    );
    if (!approved) {
      onToast("已取消插件更新。");
      return;
    }

    setUpdatingPluginId(plugin.id);
    try {
      const result = await command<PluginUpdateResult>("update_plugin_from_git", {
        pluginId: plugin.id,
        expectedCommit: update.latestCommit,
      });
      const next = await command<PluginInfo[]>("list_plugins");
      onPluginsChanged(next);
      setUpdateChecks((current) => {
        const { [plugin.id]: _updated, ...remaining } = current;
        return remaining;
      });
      onToast(result.updated ? `${plugin.name} 已更新并锁定到新的 Git commit。` : `${plugin.name} 已经是最新版本。`);
    } catch (error) {
      const message = error instanceof Error ? error.message : "插件更新失败。已保留当前已安装版本。";
      // A moving branch/tag is rejected by the host when it no longer matches
      // the reviewed commit. Invalidate the preview so the next click performs
      // a fresh explicit check rather than retrying an unseen revision.
      setUpdateChecks((current) => ({
        ...current,
        [plugin.id]: { status: "error", message },
      }));
      onToast(message);
    } finally {
      setUpdatingPluginId((current) => (current === plugin.id ? null : current));
    }
  };

  const refreshPluginsAfterLifecycleAction = async () => {
    const next = await command<PluginInfo[]>("list_plugins");
    onPluginsChanged(next);
    return next;
  };

  const setPluginEnabled = async (plugin: PluginInfo, enabled: boolean) => {
    if (!isDesktop()) {
      onToast("浏览器预览不会修改本机插件的启用状态。");
      return;
    }
    if (!enabled) {
      const approved = window.confirm(
        `停用“${plugin.name}”？\n\n插件文件会保留在本机，但其前端、命令和搜索提供器会立刻停止参与 iHub。之后可随时重新启用。`,
      );
      if (!approved) {
        onToast("已取消停用插件。");
        return;
      }
    }

    setLifecycleAction({ pluginId: plugin.id, kind: enabled ? "enable" : "disable" });
    try {
      const result = await command<PluginLifecycleUpdate>("set_plugin_enabled", {
        pluginId: plugin.id,
        enabled,
      });
      await refreshPluginsAfterLifecycleAction();
      if (!enabled) {
        setUpdateChecks((current) => {
          const { [plugin.id]: _disabled, ...remaining } = current;
          return remaining;
        });
      }
      onToast(
        result.enabled
          ? `${result.plugin.name} 已启用。`
          : `${result.plugin.name} 已停用；文件已保留在本机。`,
      );
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法更新插件启用状态。");
    } finally {
      setLifecycleAction((current) => current?.pluginId === plugin.id ? null : current);
    }
  };

  const unlinkLocalPlugin = async (plugin: PluginInfo) => {
    if (!isDesktop()) {
      onToast("浏览器预览不会修改本机开发链接。");
      return;
    }
    const staleDetail = plugin.localLinkStatus === "stale"
      ? `\n\n${displayLocalPath(plugin.localLinkError ?? "该源码链接已失效。")}`
      : "";
    const fallbackDetail = plugin.usesManagedSnapshotFallback
      ? "\n解除后仍会保留并继续使用当前受管快照。"
      : "\n解除后可重新安装该插件。";
    const approved = window.confirm(
      `解除“${plugin.name}”的本地开发链接？${staleDetail}\n\n仅移除 iHub 的链接记录；不会删除或改动：\n${displayLocalPath(plugin.localPath ?? "开发项目目录")}${fallbackDetail}`,
    );
    if (!approved) {
      onToast("已取消解除本地链接。");
      return;
    }

    setLifecycleAction({ pluginId: plugin.id, kind: "unlink" });
    try {
      await command<void>("unlink_plugin_from_local", { pluginId: plugin.id });
      const next = await refreshPluginsAfterLifecycleAction();
      if (!next.some((entry) => entry.id === plugin.id)) {
        setSelectedInstalledId(null);
      }
      onToast("本地开发链接已解除；开发项目目录未被删除。");
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法解除本地开发链接。");
    } finally {
      setLifecycleAction((current) => current?.pluginId === plugin.id ? null : current);
    }
  };

  const uninstallManagedPlugin = async (plugin: PluginInfo) => {
    if (!isDesktop()) {
      onToast("浏览器预览不会移除本机受管插件快照。");
      return;
    }
    const source = plugin.sourceLock?.source ?? plugin.source ?? "保存的 Git 来源";
    const approved = window.confirm(
      `卸载“${plugin.name}”？\n\n只会删除 iHub 受管的 Git 快照：\n${source}\n\n不会删除本地开发项目。若该 ID 当前被本地开发链接占用，需要先解除链接。`,
    );
    if (!approved) {
      onToast("已取消卸载插件。");
      return;
    }

    setLifecycleAction({ pluginId: plugin.id, kind: "uninstall" });
    try {
      const result = await command<PluginUninstallResult>("uninstall_managed_plugin", {
        pluginId: plugin.id,
      });
      await refreshPluginsAfterLifecycleAction();
      setSelectedInstalledId((current) => current === plugin.id ? null : current);
      setUpdateChecks((current) => {
        const { [plugin.id]: _removed, ...remaining } = current;
        return remaining;
      });
      onToast(`${result.pluginName} 的受管 Git 快照已卸载。`);
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法卸载受管插件快照。");
    } finally {
      setLifecycleAction((current) => current?.pluginId === plugin.id ? null : current);
    }
  };

  const handleEntryAction = (item: MarketplaceItem) => {
    const { entry, installed } = item;
    if (installed) {
      if (installed.localLinkStatus === "stale" && !installed.usesManagedSnapshotFallback) {
        onToast(displayLocalPath(
          installed.localLinkError ?? `${entry.name} 的本地源码链接已失效；解除链接后即可重新安装。`,
        ));
        return;
      }
      if (installed.enabled === false) {
        onToast(`${entry.name} 已停用；请先在插件中心启用。`);
        return;
      }
      if (installed.frontendEntry && onOpenFrontend) {
        onOpenFrontend(installed);
      } else {
        onToast(`${entry.name} 已安装，可从主命令框中搜索它的命令。`);
      }
      return;
    }
    const platformNotice = unsupportedPlatformNotice(entry, hostTarget);
    if (platformNotice) {
      onToast(platformNotice.title);
      return;
    }
    const workspaceAvailable = Boolean(
      entry.workspaceProject
      && desktopRuntime
      && workspaceProjectsLoaded
      && !workspaceProbeError
      && officialWorkspaceProjects[entry.id]?.available,
    );
    switch (preferredPluginAcquisition(entry, workspaceAvailable)) {
      case "builtin":
        handleBuiltin(entry);
        return;
      case "workspace":
        void linkOfficialWorkspacePlugin(entry);
        return;
      case "remote":
        if (entry.source) {
          void installFromSource(entry.source, entry);
        }
        return;
      case "pending":
        if (entry.workspaceProject) {
          const project = officialWorkspaceProjects[entry.id];
          onToast(project?.detail ?? workspaceProbeError ?? `${entry.name} 的本机源码项目当前不可用。`);
          return;
        }
        onToast(`${entry.name} 已加入官方 catalog，发布包仍在准备中。`);
        return;
    }
  };

  const chooseLauncherContextTarget = (target: LauncherContextEligibleCommand) => {
    if (!launcherContext) {
      return;
    }
    setPendingLauncherContextTarget(target);
  };

  const confirmLauncherContextTarget = async () => {
    const target = pendingLauncherContextTarget;
    if (!launcherContext || !target || !onRequestLauncherContextHandoff) {
      return;
    }
    const stillEligible = eligibleLauncherContextCommands([target.plugin], launcherContext)
      .some(({ command }) => command.id === target.command.id);
    if (!stillEligible) {
      setPendingLauncherContextTarget(null);
      onToast("该插件命令不再满足本次上下文权限；未共享任何内容。");
      return;
    }
    setRequestingLauncherContext(true);
    try {
      // This is intentionally the only call site that asks App to prepare a
      // transfer. Candidate rendering and the first “选择” click are inert.
      await onRequestLauncherContextHandoff(target.plugin, target.command);
      setPendingLauncherContextTarget(null);
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法准备该上下文交接；未共享任何内容。");
    } finally {
      setRequestingLauncherContext(false);
    }
  };

  const selectInstalled = (plugin: PluginInfo) => {
    setSelectedInstalledId(plugin.id);
    setFilter("installed");
    setQuery("");
    setIsAllCatalogExpanded(false);
  };

  return (
    <AnimatePresence>
      {open ? (
        <>
          <motion.button
            aria-label="关闭插件中心"
            className="plugin-center__scrim"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            onClick={onClose}
            type="button"
          />
          <motion.section
            aria-label="iHub 插件中心"
            aria-modal="true"
            className="plugin-center"
            initial={{ opacity: 0, scale: 0.985, y: 10 }}
            animate={{ opacity: 1, scale: 1, y: 0 }}
            exit={{ opacity: 0, scale: 0.985, y: 8 }}
            role="dialog"
            transition={{ type: "spring", stiffness: 380, damping: 34 }}
            >
              <style>{pluginCenterStyles}</style>
              <header className="plugin-center__topbar">
                <div
                  aria-label={`长按 ${WINDOW_DRAG_LONG_PRESS_MS} 毫秒后拖动窗口`}
                  className={`plugin-center__drag-zone${dragPending ? " is-armed" : ""}`}
                  data-drag-long-press-ms={WINDOW_DRAG_LONG_PRESS_MS}
                  data-window-drag-handle=""
                  onLostPointerCapture={(event) => windowDragControllerRef.current?.cancel(event.pointerId)}
                  onPointerCancel={(event) => windowDragControllerRef.current?.cancel(event.pointerId)}
                  onPointerDown={handleWindowDragPointerDown}
                  onPointerMove={handleWindowDragPointerMove}
                  onPointerUp={(event) => windowDragControllerRef.current?.cancel(event.pointerId)}
                  title={`长按 ${WINDOW_DRAG_LONG_PRESS_MS} 毫秒后拖动窗口`}
                />
                <div className="plugin-center__crumbs">
                <span className="plugin-center__crumb-mark"><Puzzle size={11} /></span>
                <span className="plugin-center__crumb-title">管理中心</span>
                <span className="plugin-center__crumb-separator">/</span>
                <span className="plugin-center__crumb-title plugin-center__crumb-current">{launcherContext ? "插件上下文" : "插件应用市场"}</span>
                <button aria-label="关闭插件中心" className="plugin-center__crumb-close" onClick={onClose} type="button">
                  <X size={11} />
                </button>
              </div>
              <label className="plugin-center__search">
                <Search aria-hidden="true" size={10} />
                <input
                  aria-label={launcherContext ? "筛选可接收上下文的插件命令" : "搜索插件中心"}
                  autoFocus
                  onChange={(event) => setQuery(event.target.value)}
                  placeholder={launcherContext ? "筛选已声明权限的插件命令…" : "搜索官方插件与内置工具…"}
                  value={query}
                />
              </label>
              <div className="plugin-center__top-actions">
                <div className="plugin-center__action-menu-shell" ref={actionMenuShellRef}>
                  <button
                    aria-controls="plugin-center-actions-menu"
                    aria-expanded={isActionMenuOpen}
                    aria-haspopup="menu"
                    aria-label="插件中心操作"
                    className="plugin-center__action-menu-trigger"
                    onClick={() => {
                      setIsActionMenuOpen((current) => !current);
                      setIsImportOpen(false);
                    }}
                    ref={actionMenuTriggerRef}
                    title="插件中心操作"
                    type="button"
                  >
                    <EllipsisVertical aria-hidden="true" size={13} />
                  </button>
                  <AnimatePresence initial={false}>
                    {isActionMenuOpen ? (
                      <motion.div
                        aria-label="插件中心操作"
                        className="plugin-center__action-menu"
                        exit={shouldReduceMotion ? { opacity: 0 } : { opacity: 0, scale: 0.96, y: -3 }}
                        id="plugin-center-actions-menu"
                        initial={shouldReduceMotion ? false : { opacity: 0, scale: 0.96, y: -3 }}
                        animate={{ opacity: 1, scale: 1, y: 0 }}
                        onKeyDown={(event) => {
                          if (!["ArrowDown", "ArrowUp", "Home", "End"].includes(event.key)) {
                            return;
                          }
                          event.preventDefault();
                          const items = Array.from(event.currentTarget.querySelectorAll<HTMLButtonElement>('[role="menuitem"]'));
                          const currentIndex = items.indexOf(document.activeElement as HTMLButtonElement);
                          const nextIndex = event.key === "Home"
                            ? 0
                            : event.key === "End"
                              ? items.length - 1
                              : event.key === "ArrowUp"
                                ? (currentIndex - 1 + items.length) % items.length
                                : (currentIndex + 1) % items.length;
                          items[nextIndex]?.focus();
                        }}
                        role="menu"
                        transition={shouldReduceMotion ? { duration: 0 } : { duration: 0.14, ease: "easeOut" }}
                      >
                        <button
                          className="plugin-center__action-menu-item"
                          onClick={openGitHubImport}
                          ref={firstActionMenuItemRef}
                          role="menuitem"
                          type="button"
                        >
                          <Download aria-hidden="true" size={11} />
                          <span>从 GitHub 导入插件</span>
                        </button>
                        <div className="plugin-center__action-menu-separator" role="separator" />
                        <button
                          className="plugin-center__action-menu-item"
                          onClick={openPluginProjectCreator}
                          role="menuitem"
                          type="button"
                        >
                          <Code2 aria-hidden="true" size={11} />
                          <span>创建插件项目</span>
                        </button>
                        <button
                          className="plugin-center__action-menu-item"
                          onClick={openPluginCenterSettings}
                          role="menuitem"
                          type="button"
                        >
                          <Settings2 aria-hidden="true" size={11} />
                          <span>偏好设置</span>
                        </button>
                      </motion.div>
                    ) : null}
                  </AnimatePresence>
                  <AnimatePresence initial={false}>
                    {isImportOpen ? (
                      <motion.form
                        aria-label="从 GitHub 导入插件"
                        className="plugin-center__import-popover"
                        exit={shouldReduceMotion ? { opacity: 0 } : { opacity: 0, y: -5 }}
                        initial={shouldReduceMotion ? false : { opacity: 0, y: -5 }}
                        animate={{ opacity: 1, y: 0 }}
                        onSubmit={(event) => {
                          event.preventDefault();
                          void installFromSource(importSource);
                        }}
                        transition={shouldReduceMotion ? { duration: 0 } : { duration: 0.14, ease: "easeOut" }}
                      >
                        <label htmlFor="plugin-center-import">从 GitHub 导入插件</label>
                        <p>支持 owner/repo@tag、github:owner/repo@tag 与完整仓库链接（可附 #ref）。导入只读取已提交的构建产物，不会安装依赖或执行项目脚本。</p>
                        <div className="plugin-center__import-field">
                          <input
                            autoCapitalize="none"
                            autoCorrect="off"
                            aria-describedby="plugin-center-import-hint"
                            aria-invalid={Boolean(importSource.trim()) && !importSourceAnalysis.isPlausible}
                            id="plugin-center-import"
                            onChange={(event) => setImportSource(event.target.value)}
                            placeholder="owner/ihub-plugin-name@v1.2.0"
                            spellCheck="false"
                            value={importSource}
                          />
                          <button
                            aria-label="安装 GitHub 插件"
                            className={`plugin-center__import-submit${installingSource ? " is-loading" : ""}`}
                            disabled={installingSource !== null || !importSourceAnalysis.isPlausible}
                            type="submit"
                          >
                            {installingSource ? <LoaderCircle className="spin" size={9} /> : <Download size={9} />}
                          </button>
                        </div>
                        <p
                          className={`plugin-center__import-hint ${importSourceAnalysis.isPlausible ? "is-ready" : "is-invalid"}`}
                          id="plugin-center-import-hint"
                          role="status"
                        >
                          {importSourceAnalysis.hint}
                        </p>
                      </motion.form>
                    ) : null}
                  </AnimatePresence>
                </div>
                <button
                  aria-label="打开个人中心"
                  className="plugin-center__hub-mark"
                  onClick={() => onToast("个人中心会在账号体系启用后提供；当前插件与数据仍只保存在本机。")}
                  title="个人中心"
                  type="button"
                >
                  <Puzzle size={17} />
                </button>
              </div>
            </header>

            <div className="plugin-center__body">
              <aside className={`plugin-center__sidebar${launcherContext ? " is-context" : ""}`}>
                {launcherContext ? (
                  <>
                    <div className="plugin-center__context-sidebar-copy">
                      <p className="plugin-center__context-kicker">EXPLICIT HANDOFF</p>
                      <h3>本次共享</h3>
                      <p>{launcherContext.title}</p>
                    </div>
                    <p className="plugin-center__context-sidebar-note">
                      这里只列出已启用、已安装且声明匹配权限的前端命令。浏览、搜索或渲染此列表不会发送任何内容。
                    </p>
                    <button className="plugin-center__context-cancel" onClick={onClose} type="button">
                      取消本次交接
                    </button>
                  </>
                ) : (
                  <>
                <div className="plugin-center__side-heading">
                  <span>已安装与内置工具</span>
                  <small>{sidebarEntries.length}</small>
                </div>
                <section className="plugin-center__installed" aria-label="已安装插件与内置工具">
                  <div className="plugin-center__installed-list">
                    {sidebarEntries.map((item) => {
                      const Icon = iconForCatalog[item.entry.icon];
                      const artworkSrc = safePluginArtworkSrc(item.installed?.iconSrc);
                      const isSelected = Boolean(item.installed && selectedInstalledId === item.installed.id);
                      return (
                        <button
                          className={"plugin-center__installed-item" + (isSelected ? " is-selected" : "") + (item.installed?.enabled === false ? " is-disabled" : "")}
                          key={item.entry.id}
                          onClick={() => {
                            if (item.installed) {
                              selectInstalled(item.installed);
                            } else {
                              handleEntryAction(item);
                            }
                          }}
                          type="button"
                        >
                          <span className={`plugin-center__installed-icon plugin-center__installed-icon--${item.entry.icon}${artworkSrc ? " is-artwork" : ""}`}>
                            <PluginArtwork
                              fallback={<Icon size={10} />}
                              iconSrc={artworkSrc}
                            />
                          </span>
                          <span className="plugin-center__installed-copy">
                            <strong>{item.installed?.name ?? item.entry.name}</strong>
                            <small title={[item.installed ? lifecycleTitle(item.installed) : undefined, sourceLockTitle(item.installed)].filter(Boolean).join("\n\n") || undefined}>
                              {item.installed
                                ? `v${item.installed.version} · ${
                                  item.installed.localLinkStatus === "stale"
                                    ? item.installed.usesManagedSnapshotFallback
                                      ? "源码失效 · 快照回退"
                                      : "源码失效"
                                    : item.installed.enabled === false
                                      ? "已停用"
                                      : item.installed.isDevelopmentLink
                                        ? "本地链接"
                                        : "已启用"
                                }${sourceLockLabel(item.installed) ? ` · ${sourceLockLabel(item.installed)}` : ""}`
                                : "内置工具"}
                            </small>
                          </span>
                        </button>
                      );
                    })}
                  </div>
                </section>

                <div className="plugin-center__side-footer">
                  <button
                    className="plugin-center__profile-link"
                    onClick={() => onToast("个人中心会在账号体系启用后提供；当前插件与数据仍只保存在本机。")}
                    type="button"
                  >
                    <img alt="" src="/ihub-avatar.svg" />
                    <span>个人中心</span>
                    <ChevronRight size={10} />
                  </button>
                  <button
                    aria-label="创建插件项目"
                    className="plugin-center__developer-link"
                    onClick={() => handleBuiltin(pluginCatalog.find((entry) => entry.builtinTool === "developer")!)}
                    title="创建插件项目"
                    type="button"
                  >
                    <Code2 size={11} />
                  </button>
                  {onOpenSettings ? (
                    <button
                      aria-label="打开偏好设置"
                      className="plugin-center__developer-link"
                      onClick={onOpenSettings}
                      title="偏好设置"
                      type="button"
                    >
                      <Settings2 size={11} />
                    </button>
                  ) : null}
                </div>
                  </>
                )}
              </aside>

              <main className={`plugin-center__main${launcherContext ? " plugin-center__context-main" : ""}`}>
                <div className="plugin-center__page-body">
                {launcherContext ? (
                  <section aria-label="选择可接收本次上下文的插件命令" className="plugin-center__context-panel">
                    <header className="plugin-center__context-heading">
                      <div className="plugin-center__context-heading-top">
                        <span className="plugin-center__context-shield"><ShieldCheck aria-hidden="true" size={13} /></span>
                        <div>
                          <p className="plugin-center__context-kicker">USER-CONFIRMED PLUGIN ACTION</p>
                          <h2>选择处理“{launcherContext.suggestedUse}”的插件命令</h2>
                          <p>{launcherContext.title} · {launcherContext.detail}</p>
                        </div>
                      </div>
                      <div className="plugin-center__context-scope">
                        {launcherContext.categories.map((category) => (
                          <span key={category}>将共享：{launcherContextCategoryLabel(category)}</span>
                        ))}
                        <small>不会读取剪贴板、路径或图片像素。</small>
                      </div>
                    </header>

                    {launcherContextCandidates.length ? (
                      <div className="plugin-center__context-list" role="list">
                        {launcherContextCandidates.map((target) => (
                          <article className="plugin-center__context-command" key={`${target.plugin.id}:${target.command.id}`} role="listitem">
                            <div className="plugin-center__context-command-copy">
                              <strong>
                                {target.plugin.name}
                                <span>{target.command.name || target.command.id}</span>
                              </strong>
                              <p>{target.command.description || target.plugin.description || "已声明可接收本次上下文的前端命令。"}</p>
                              <small>
                                frontend · 已声明 {launcherContext.categories.map(launcherContextCategoryLabel).join("、")}
                              </small>
                            </div>
                            <button
                              className="plugin-center__context-command-select"
                              onClick={() => chooseLauncherContextTarget(target)}
                              type="button"
                            >
                              选择此命令
                            </button>
                          </article>
                        ))}
                      </div>
                    ) : (
                      <div className="plugin-center__context-empty">
                        <strong>没有可接收本次内容的已安装插件命令</strong>
                        只会显示已启用、带前端入口，并在清单中声明匹配 <code>launcherContext</code> 权限的命令。安装或启用兼容插件后，请重新从启动器选择该操作。
                      </div>
                    )}
                  </section>
                ) : (
                  <>
                <nav aria-label="插件类别" className="plugin-center__catalog-filters">
                  <button
                    aria-pressed={filter === "all"}
                    className={"plugin-center__catalog-filter" + (filter === "all" ? " is-active" : "")}
                    onClick={() => {
                      setFilter("all");
                      setSelectedInstalledId(null);
                      setQuery("");
                      setIsAllCatalogExpanded(true);
                    }}
                    type="button"
                  >
                    <span>全部插件</span><small>{pluginCatalog.length}</small>
                  </button>
                  <button
                    aria-pressed={filter === "installed"}
                    className={"plugin-center__catalog-filter" + (filter === "installed" ? " is-active" : "")}
                    onClick={() => {
                      setFilter("installed");
                      setSelectedInstalledId(null);
                      setQuery("");
                      setIsAllCatalogExpanded(false);
                    }}
                    type="button"
                  >
                    <span>已安装</span><small>{installedEntries.length}</small>
                  </button>
                  {pluginCatalogCategories.map((category) => (
                    <button
                      aria-pressed={filter === category.id}
                      className={"plugin-center__catalog-filter" + (filter === category.id ? " is-active" : "")}
                      key={category.id}
                      onClick={() => {
                        setFilter(category.id);
                        setSelectedInstalledId(null);
                        setQuery("");
                        setIsAllCatalogExpanded(false);
                      }}
                      type="button"
                    >
                      <span>{category.label}</span><small>{categoryCounts.get(category.id) ?? 0}</small>
                    </button>
                  ))}
                </nav>

                <div className="plugin-center__market-header">
                  <div>
                    <h2>{filter === "installed" ? "已安装" : query ? "搜索结果" : "插件应用市场"}</h2>
                    <p>{filter === "installed" ? "管理当前设备上可用的插件。" : "浏览、筛选内置工具与可安装插件，或直接导入 GitHub 项目。"}</p>
                  </div>
                  <span className="plugin-center__market-count">{visibleItems.length} 项</span>
                </div>

                {isDefaultMarketplace ? (
                  <div className="plugin-center__featured-heading">
                    <h3>{isCatalogPreview ? "插件列表" : "全部插件"}</h3>
                    <div className="plugin-center__featured-actions">
                      <button
                        aria-controls="plugin-center-catalog-items"
                        aria-expanded={!isCatalogPreview}
                        className="plugin-center__catalog-toggle"
                        onClick={() => setIsAllCatalogExpanded((current) => !current)}
                        type="button"
                      >
                        {isCatalogPreview ? "查看全部" : "收起"}
                        <span className="plugin-center__catalog-toggle-count">{visibleItems.length}</span>
                        {isCatalogPreview
                          ? <ChevronRight aria-hidden="true" size={9} />
                          : <ChevronUp aria-hidden="true" size={9} />}
                      </button>
                    </div>
                  </div>
                ) : null}

                {displayedItems.length ? (
                  <section
                    aria-label="插件列表"
                    className="plugin-center__market-grid"
                    id="plugin-center-catalog-items"
                  >
                    {displayedItems.map((item) => {
                      const Icon = iconForCatalog[item.entry.icon];
                      const plugin = item.installed;
                      const artworkSrc = safePluginArtworkSrc(plugin?.iconSrc);
                      const installed = Boolean(plugin);
                      const workspaceProject = item.entry.workspaceProject
                        ? officialWorkspaceProjects[item.entry.id]
                        : undefined;
                      const workspaceChecking = Boolean(
                        item.entry.workspaceProject
                        && desktopRuntime
                        && !workspaceProjectsLoaded,
                      );
                      const workspaceAvailable = Boolean(
                        item.entry.workspaceProject
                        && desktopRuntime
                        && workspaceProjectsLoaded
                        && !workspaceProbeError
                        && workspaceProject?.available,
                      );
                      const workspaceRequiredUnavailable = Boolean(
                        item.entry.distribution === "bootstrap"
                        && item.entry.workspaceProject
                        && (
                          !desktopRuntime
                          || (
                            workspaceProjectsLoaded
                            && (!workspaceProject?.available || Boolean(workspaceProbeError))
                          )
                        ),
                      );
                      const isLinkingWorkspace = linkingWorkspacePluginId === item.entry.id;
                      const pending = item.entry.distribution === "bootstrap"
                        && !item.entry.workspaceProject
                        && !installed;
                      const platformNotice = installed ? null : unsupportedPlatformNotice(item.entry, hostTarget);
                      const isInstalling = installingSource === item.entry.source;
                      const isGitPlugin = plugin ? isGitInstalledPlugin(plugin) : false;
                      const automaticDiscovery = hasAutomaticUpdateDiscovery(plugin);
                      const update = plugin ? updateChecks[plugin.id] : undefined;
                      const isCheckingUpdate = plugin?.id === checkingPluginId;
                      const isApplyingUpdate = plugin?.id === updatingPluginId;
                      // `undefined === undefined` made every catalog entry
                      // without an installed plugin look busy, permanently
                      // disabling its Open/Install action.
                      const isLifecycleBusy = Boolean(plugin && lifecycleAction?.pluginId === plugin.id);
                      const lifecycleBusyElsewhere = lifecycleAction !== null && !isLifecycleBusy;
                      const updateLabel = updateStatusLabel(update, isCheckingUpdate, isApplyingUpdate);
                      const shortcutStatus = pluginShortcutStatusSummary(plugin);
                      const updateTitle = [
                        sourceLockTitle(plugin),
                        plugin ? lifecycleTitle(plugin) : undefined,
                        automaticDiscovery ? "插件中心打开时按需检查，保持打开期间每 30 分钟复查：仅发现可信 stable Git commit；会先验证已安装快照的内容哈希，且不会自动下载或应用更新。" : undefined,
                        plugin && isGitPlugin ? updateActionTitle(update) : undefined,
                      ].filter(Boolean).join("\n\n") || undefined;
                      const statusTitle = [platformNotice?.title, updateTitle, shortcutStatus?.title].filter(Boolean).join("\n\n") || undefined;
                      const workspaceStatusTitle = item.entry.workspaceProject
                        ? workspaceAvailable || item.entry.distribution === "bootstrap"
                          ? workspaceProject?.detail
                            ?? workspaceProbeError
                            ?? (desktopRuntime
                              ? "正在验证开发安装器记录的当前 iHub 源码工作区。"
                              : "浏览器预览和普通网页不能链接本机源码项目。")
                          : undefined
                        : undefined;
                      const canOpenInstalled = Boolean(plugin?.enabled !== false && plugin?.frontendEntry && onOpenFrontend);
                      let actionLabel = "安装";
                      if (installed) {
                        actionLabel = plugin?.localLinkStatus === "stale" && !plugin.usesManagedSnapshotFallback
                          ? "需解除链接"
                          : plugin?.enabled === false
                            ? "已停用"
                            : canOpenInstalled
                              ? "打开"
                              : "已安装";
                      } else if (item.entry.distribution === "builtin") {
                        actionLabel = "打开";
                      } else if (workspaceChecking) {
                        actionLabel = "检查本机源码";
                      } else if (workspaceAvailable) {
                        actionLabel = isLinkingWorkspace ? "链接中" : "链接源码";
                      } else if (platformNotice) {
                        actionLabel = "当前设备不支持";
                      } else if (workspaceRequiredUnavailable) {
                        actionLabel = desktopRuntime ? "源码不可用" : "仅桌面开发版";
                      } else if (pending) {
                        actionLabel = "筹备中";
                      } else if (isInstalling) {
                        actionLabel = "安装中";
                      }
                      return (
                        <article className={"plugin-center__market-item" + (plugin?.enabled === false ? " is-disabled" : "") + (platformNotice ? " is-platform-unsupported" : "")} key={item.entry.id}>
                          <span className={`plugin-center__market-icon plugin-center__market-icon--${item.entry.icon}${artworkSrc ? " is-artwork" : ""}`}>
                            <PluginArtwork
                              fallback={<Icon size={12} />}
                              iconSrc={artworkSrc}
                            />
                          </span>
                          <div className="plugin-center__market-copy">
                            <strong>{item.entry.name}</strong>
                            <p>{item.entry.description}</p>
                            <small title={[plugin?.localLinkError ? displayLocalPath(plugin.localLinkError) : null, statusTitle].filter(Boolean).join("\n\n") || undefined}>
                              {platformNotice?.status ?? statusLabel(item.entry, plugin, workspaceProject, desktopRuntime, workspaceProjectsLoaded)}{item.entry.native && !installed ? " · 原生能力" : ""}{sourceLockLabel(plugin) ? ` · 锁定 ${sourceLockLabel(plugin)}` : ""}{automaticDiscovery ? " · 官方仅自动检查" : ""}{updateLabel ? ` · ${updateLabel}` : ""}
                              {plugin?.searchProviders?.length ? ` · 已声明 ${plugin.searchProviders.length} 个搜索提供器` : ""}
                              {shortcutStatus ? ` · ${shortcutStatus.label}` : ""}
                            </small>
                          </div>
                          <div className="plugin-center__market-actions">
                            {plugin && isGitPlugin && plugin.enabled !== false ? (
                              <button
                                aria-label={`${plugin.name}：${updateActionLabel(update, isCheckingUpdate, isApplyingUpdate)}`}
                                className={"plugin-center__update-action" + (update?.status === "available" ? " is-available" : "") + (update?.status === "up-to-date" ? " is-current" : "")}
                                disabled={isCheckingUpdate || isApplyingUpdate || Boolean(lifecycleAction)}
                                onClick={() => {
                                  if (update?.status === "available") {
                                    void applyPluginUpdate(plugin);
                                  } else {
                                    void checkPluginUpdate(plugin);
                                  }
                                }}
                                title={updateActionTitle(update) ?? "查询远程 Git 源是否有新 commit。"}
                                type="button"
                              >
                                {isCheckingUpdate || isApplyingUpdate ? (
                                  <LoaderCircle className="spin" size={9} />
                                ) : update?.status === "available" ? (
                                  <Download size={9} />
                                ) : (
                                  <RefreshCw size={9} />
                                )}
                                {updateActionLabel(update, isCheckingUpdate, isApplyingUpdate)}
                              </button>
                            ) : null}
                            {plugin && !plugin.isDevelopmentLink && workspaceAvailable ? (
                              <button
                                aria-label={`${plugin.name}：切换到当前源码`}
                                className="plugin-center__lifecycle-action is-link"
                                disabled={Boolean(lifecycleAction) || isCheckingUpdate || isApplyingUpdate || isLinkingWorkspace}
                                onClick={() => void linkOfficialWorkspacePlugin(item.entry)}
                                title="保留当前受管 Git 快照作为回退，并优先读取这个可信 iHub checkout 中的构建产物。"
                                type="button"
                              >
                                {isLinkingWorkspace ? <LoaderCircle className="spin" size={9} /> : <FolderSearch size={9} />}
                                {isLinkingWorkspace ? "链接中" : "切到源码"}
                              </button>
                            ) : null}
                            {plugin ? (
                              <button
                                aria-label={`${plugin.name}：${plugin.enabled === false ? "启用" : "停用"}`}
                                className={"plugin-center__lifecycle-action" + (plugin.enabled === false ? " is-enable" : "")}
                                disabled={Boolean(lifecycleAction) || isCheckingUpdate || isApplyingUpdate || (plugin.localLinkStatus === "stale" && !plugin.usesManagedSnapshotFallback)}
                                onClick={() => void setPluginEnabled(plugin, plugin.enabled === false)}
                                title={plugin.localLinkStatus === "stale" && !plugin.usesManagedSnapshotFallback
                                  ? "该链接没有可运行的源码或受管快照；请先解除链接。"
                                  : plugin.enabled === false
                                    ? "启用插件并恢复其前端、命令和搜索提供器。"
                                    : "停用插件，但保留本机文件与来源记录。"}
                                type="button"
                              >
                                {isLifecycleBusy && (lifecycleAction?.kind === "enable" || lifecycleAction?.kind === "disable") ? (
                                  <LoaderCircle className="spin" size={9} />
                                ) : (
                                  <Power size={9} />
                                )}
                                {plugin.enabled === false ? "启用" : "停用"}
                              </button>
                            ) : null}
                            {plugin?.isDevelopmentLink ? (
                              <button
                                aria-label={`${plugin.name}：解除本地开发链接`}
                                className="plugin-center__lifecycle-action is-link"
                                disabled={Boolean(lifecycleAction) || isCheckingUpdate || isApplyingUpdate}
                                onClick={() => void unlinkLocalPlugin(plugin)}
                                title="仅移除 iHub 的本地链接记录；不会删除开发项目目录。"
                                type="button"
                              >
                                {isLifecycleBusy && lifecycleAction?.kind === "unlink" ? <LoaderCircle className="spin" size={9} /> : <X size={9} />}
                                解除链接
                              </button>
                            ) : plugin && isGitPlugin ? (
                              <button
                                aria-label={`${plugin.name}：卸载受管 Git 快照`}
                                className="plugin-center__lifecycle-action is-danger"
                                disabled={Boolean(lifecycleAction) || isCheckingUpdate || isApplyingUpdate}
                                onClick={() => void uninstallManagedPlugin(plugin)}
                                title="只删除 iHub 受管的 Git 快照；本地开发目录不会成为删除目标。"
                                type="button"
                              >
                                {isLifecycleBusy && lifecycleAction?.kind === "uninstall" ? <LoaderCircle className="spin" size={9} /> : <Trash2 size={9} />}
                                卸载
                              </button>
                            ) : null}
                            <button
                              className={"plugin-center__market-action" + (installed && !canOpenInstalled ? " is-installed" : "") + (pending ? " is-pending" : "") + (platformNotice ? " is-platform-unsupported" : "")}
                              disabled={Boolean(platformNotice) || pending || workspaceChecking || workspaceRequiredUnavailable || isLinkingWorkspace || (installed && !canOpenInstalled) || isInstalling || isLifecycleBusy || lifecycleBusyElsewhere}
                              onClick={() => handleEntryAction(item)}
                              title={(plugin?.localLinkError ? displayLocalPath(plugin.localLinkError) : undefined) ?? platformNotice?.title ?? workspaceStatusTitle ?? (pending ? "该官方插件已备案，尚未发布可安装产物。" : undefined)}
                              type="button"
                            >
                              {isInstalling || isLinkingWorkspace || workspaceChecking
                                ? <LoaderCircle className="spin" size={9} />
                                : installed
                                  ? plugin?.localLinkStatus === "stale" && !plugin.usesManagedSnapshotFallback
                                    ? <X size={9} />
                                    : <Check size={9} />
                                  : workspaceAvailable
                                    ? <FolderSearch size={9} />
                                    : item.entry.distribution === "installable" && !platformNotice
                                      ? <Download size={9} />
                                      : null}
                              {actionLabel}
                            </button>
                          </div>
                        </article>
                      );
                    })}
                  </section>
                ) : (
                  <div className="plugin-center__empty">
                    <strong>没有找到匹配的插件</strong>
                    换一个关键词，或使用右上角的 GitHub 导入安装你信任的项目。
                  </div>
                )}
                  </>
                )}
                </div>
              </main>
            </div>
            <AnimatePresence initial={false}>
              {launcherContext && pendingLauncherContextTarget ? (
                <motion.div
                  aria-describedby="plugin-context-confirm-copy"
                  aria-labelledby="plugin-context-confirm-title"
                  aria-modal="true"
                  className="plugin-center__context-confirm"
                  exit={shouldReduceMotion ? { opacity: 0 } : { opacity: 0, scale: 0.98 }}
                  initial={shouldReduceMotion ? false : { opacity: 0, scale: 0.98 }}
                  animate={{ opacity: 1, scale: 1 }}
                  role="dialog"
                >
                  <div className="plugin-center__context-confirm-card">
                    <span className="plugin-center__context-confirm-eyebrow">ONE-TIME HANDOFF</span>
                    <h2 id="plugin-context-confirm-title">
                      交给“{pendingLauncherContextTarget.plugin.name} / {pendingLauncherContextTarget.command.name || pendingLauncherContextTarget.command.id}”吗？
                    </h2>
                    <p id="plugin-context-confirm-copy">
                      只有确认后，iHub 才会为这个前端命令签发一次、短时有效的上下文 ID。
                    </p>
                    <div className="plugin-center__context-confirm-scope">
                      <strong>将共享：{launcherContext.title}</strong>
                      <p>{launcherContext.detail}</p>
                    </div>
                    <p className="plugin-center__context-confirm-warning">
                      不会自动读取系统剪贴板；文件不含路径或读取权限，图片不含像素。取消不会创建或保留任何上下文。
                    </p>
                    <div className="plugin-center__context-confirm-actions">
                      <button
                        className="plugin-center__context-confirm-cancel"
                        disabled={requestingLauncherContext}
                        onClick={() => setPendingLauncherContextTarget(null)}
                        type="button"
                      >
                        取消
                      </button>
                      <button
                        className="plugin-center__context-confirm-approve"
                        disabled={requestingLauncherContext || !onRequestLauncherContextHandoff}
                        onClick={() => void confirmLauncherContextTarget()}
                        type="button"
                      >
                        {requestingLauncherContext ? <LoaderCircle className="spin" size={9} /> : <Check size={9} />}
                        {requestingLauncherContext ? "正在准备…" : "确认并运行"}
                      </button>
                    </div>
                  </div>
                </motion.div>
              ) : null}
            </AnimatePresence>
          </motion.section>
        </>
      ) : null}
    </AnimatePresence>
  );
}
