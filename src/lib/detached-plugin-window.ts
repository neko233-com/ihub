import type {
  PluginFrontendEvent,
  PluginGlobalShortcutEvent,
  PluginInfo,
} from "./types";

const DETACHED_PLUGIN_PARAMETER = "ihubDetachedPlugin";
const DETACHED_PLUGIN_PREVIEW_PARAMETER = "ihubDetachedPreview";
const PLUGIN_ID_PATTERN = /^[A-Za-z0-9._-]{2,96}$/;

export interface DetachedPluginRoute {
  kind: "detached";
  pluginId: string;
  browserPreview: boolean;
}

export interface InvalidDetachedPluginRoute {
  kind: "invalid-detached";
  message: string;
}

export interface MainApplicationRoute {
  kind: "main";
}

export type ApplicationRoute =
  | DetachedPluginRoute
  | InvalidDetachedPluginRoute
  | MainApplicationRoute;

export interface DetachedPluginWindowOpened {
  pluginId: string;
  windowLabel: string;
  created: boolean;
}

export interface PluginSurfaceShortcutEvent {
  altKey: boolean;
  ctrlKey: boolean;
  defaultPrevented: boolean;
  isComposing: boolean;
  key: string;
  metaKey: boolean;
  repeat: boolean;
  shiftKey: boolean;
}

export const DETACHED_PLUGIN_BROWSER_PREVIEW_STATUS =
  "浏览器安全预览：未创建原生窗口，未签发 loopback 租约，也未授予 Tauri、Node 或 shell 权限。";

export function isValidPluginId(value: string): boolean {
  return PLUGIN_ID_PATTERN.test(value);
}

function detachedShortcutRequestId(): string {
  const suffix =
    typeof crypto !== "undefined" && "randomUUID" in crypto
      ? crypto.randomUUID()
      : Math.random().toString(36).slice(2);
  return `detached-shortcut-${Date.now().toString(36)}-${suffix}`;
}

/**
 * Converts only an exact, enabled frontend command for this detached plugin
 * into the existing PluginFrontendFrame one-shot event path.
 *
 * The native host already targets one registry-derived window label. This
 * renderer check is a second boundary: a mismatched plugin ID, keyword search,
 * native worker command, or stale manifest command is ignored rather than
 * being redirected to another plugin or window.
 */
export function createDetachedPluginShortcutEvent(
  routePluginId: string,
  plugin: PluginInfo,
  shortcut: PluginGlobalShortcutEvent,
  requestId = detachedShortcutRequestId(),
): PluginFrontendEvent | null {
  if (
    !isValidPluginId(routePluginId)
    || plugin.id !== routePluginId
    || plugin.enabled === false
    || shortcut.pluginId !== routePluginId
    || typeof shortcut.commandId !== "string"
    || shortcut.keyword !== undefined
    || !isValidPluginId(shortcut.commandId)
    || !Array.isArray(plugin.commands)
  ) {
    return null;
  }

  const command = plugin.commands.find(
    (candidate) => candidate.id === shortcut.commandId,
  );
  if (
    !command
    || (
      command.execution !== "frontend"
      && (!plugin.frontendEntry || plugin.hasNativeWorker)
    )
  ) {
    return null;
  }

  return {
    id: requestId,
    pluginId: routePluginId,
    name: `ihub://plugin/${routePluginId}/command`,
    payload: {
      requestId,
      commandId: command.id,
      input: null,
      context: null,
    },
  };
}

/**
 * Selects the top-level React host before App mounts.
 *
 * A native detached window has one fixed query field derived by Rust. Browser
 * QA may opt into the second preview flag, which never calls a desktop API or
 * mounts a plugin iframe. Duplicate, unknown, malformed, and desktop-preview
 * fields fail closed into a trusted error surface rather than the launcher.
 */
export function parseApplicationRoute(
  search: string,
  desktop: boolean,
  hash = "",
): ApplicationRoute {
  const params = new URLSearchParams(search);
  const pluginIds = params.getAll(DETACHED_PLUGIN_PARAMETER);
  if (pluginIds.length === 0) {
    return { kind: "main" };
  }
  if (hash.length > 0) {
    return {
      kind: "invalid-detached",
      message: "分离窗口地址不能包含 fragment。",
    };
  }

  const previewValues = params.getAll(DETACHED_PLUGIN_PREVIEW_PARAMETER);
  const allowedKeys = new Set([
    DETACHED_PLUGIN_PARAMETER,
    DETACHED_PLUGIN_PREVIEW_PARAMETER,
  ]);
  if ([...params.keys()].some((key) => !allowedKeys.has(key))) {
    return {
      kind: "invalid-detached",
      message: "分离窗口地址包含宿主未签发的参数。",
    };
  }
  if (pluginIds.length !== 1 || !isValidPluginId(pluginIds[0] ?? "")) {
    return {
      kind: "invalid-detached",
      message: "分离窗口的插件标识无效。",
    };
  }
  if (
    previewValues.length > 1
    || previewValues.some((value) => value !== "1")
  ) {
    return {
      kind: "invalid-detached",
      message: "分离窗口的浏览器预览标记无效。",
    };
  }
  const browserPreview = previewValues.length === 1;
  if (desktop && browserPreview) {
    return {
      kind: "invalid-detached",
      message: "桌面分离窗口不能使用浏览器预览身份。",
    };
  }

  return {
    kind: "detached",
    pluginId: pluginIds[0]!,
    browserPreview,
  };
}

/** Ctrl+D belongs to the trusted visible plugin host only. */
export function shouldDetachPluginSurface(
  event: PluginSurfaceShortcutEvent,
  surfaceActive: boolean,
): boolean {
  return surfaceActive
    && !event.defaultPrevented
    && !event.isComposing
    && !event.repeat
    && event.ctrlKey
    && !event.metaKey
    && !event.altKey
    && !event.shiftKey
    && event.key.toLocaleLowerCase() === "d";
}
