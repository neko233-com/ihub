import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import {
  browserHostLogSnapshot,
  emptyHostLogSnapshot,
  normalizeHostLogSnapshot,
} from "./host-log";
import type { HostLogSnapshot, SuperPanelEvent } from "./types";

export const isDesktop = () =>
  typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

export async function command<T>(
  name: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (!isDesktop()) {
    throw new Error("This action is available in the iHub desktop app.");
  }

  return invoke<T>(name, args);
}

export interface LauncherFocusEventPayload {
  /** True only when a hidden launcher starts a new centered reveal. */
  freshReveal: boolean;
  reason: "hotkey" | "explicit";
}

export async function onFocusSearch(
  callback: (payload: LauncherFocusEventPayload) => void,
): Promise<UnlistenFn> {
  if (!isDesktop()) {
    return () => undefined;
  }

  return listen<LauncherFocusEventPayload>("ihub://focus-search", (event) => {
    const payload = event.payload as Partial<LauncherFocusEventPayload> | null;
    callback({
      // Older development hosts emitted `{}`. Treat that as a fresh reveal so
      // a source-first renderer can still recover from its hidden surface.
      freshReveal: payload?.freshReveal !== false,
      reason: payload?.reason === "hotkey" ? "hotkey" : "explicit",
    });
  });
}

/** Fired by the native shell after the launcher hides because it lost focus. */
export async function onHideSearch(callback: () => void): Promise<UnlistenFn> {
  if (!isDesktop()) {
    return () => undefined;
  }

  return listen("ihub://hide-search", callback);
}

export interface TrayNavigationEventPayload {
  surface: "settings";
  section: "preferences" | "about" | "shortcuts" | "ai";
  pluginId?: string;
  commandLabel?: string;
  autoCopy?: boolean;
}

export async function onTrayNavigation(
  callback: (payload: TrayNavigationEventPayload) => void,
): Promise<UnlistenFn> {
  if (!isDesktop()) {
    return () => undefined;
  }
  return listen<TrayNavigationEventPayload>("ihub://tray-navigation", (event) => {
    const payload = event.payload as Partial<TrayNavigationEventPayload> | null;
    if (payload?.surface === "settings") {
      callback({
        surface: "settings",
        section: ["about", "shortcuts", "ai"].includes(String(payload.section))
          ? payload.section as TrayNavigationEventPayload["section"]
          : "preferences",
        pluginId: typeof payload.pluginId === "string" && payload.pluginId.length <= 128
          ? payload.pluginId
          : undefined,
        commandLabel: typeof payload.commandLabel === "string" && [...payload.commandLabel].length <= 160
          ? payload.commandLabel
          : undefined,
        autoCopy: typeof payload.autoCopy === "boolean" ? payload.autoCopy : undefined,
      });
    }
  });
}

export interface PluginGlobalShortcutEventPayload {
  pluginId: string;
  shortcut: string;
  commandId?: string;
  keyword?: string;
  input?: string;
}

export async function onPluginGlobalShortcut(
  callback: (payload: PluginGlobalShortcutEventPayload) => void,
): Promise<UnlistenFn> {
  if (!isDesktop()) {
    return () => undefined;
  }
  return listen<PluginGlobalShortcutEventPayload>("ihub://plugin-global-shortcut", (event) => {
    const payload = event.payload as Partial<PluginGlobalShortcutEventPayload> | null;
    if (
      typeof payload?.pluginId !== "string"
      || typeof payload.shortcut !== "string"
      || (typeof payload.commandId === "string") === (typeof payload.keyword === "string")
      || (payload.input !== undefined && (
        typeof payload.input !== "string"
        || typeof payload.commandId !== "string"
        || payload.input.includes("\0")
        || new TextEncoder().encode(payload.input).byteLength > 48 * 1024
      ))
    ) {
      return;
    }
    callback(payload as PluginGlobalShortcutEventPayload);
  });
}

export type UtoolsRedirectAction =
  | { type: "text"; payload: string }
  | { type: "img"; payload: string }
  | { type: "files"; payload: string[] };

export interface UtoolsRedirectCandidate {
  pluginId: string;
  commandId: string;
  pluginName: string;
  commandName: string;
}

export interface UtoolsRedirectEventPayload {
  sourcePluginId: string;
  label: string;
  candidates: UtoolsRedirectCandidate[];
  action: UtoolsRedirectAction;
}

function isUtoolsRedirectAction(value: unknown): value is UtoolsRedirectAction {
  const action = value && typeof value === "object" ? value as Partial<UtoolsRedirectAction> : null;
  if (action?.type === "text") {
    return typeof action.payload === "string" && new TextEncoder().encode(action.payload).byteLength <= 48 * 1_024;
  }
  if (action?.type === "img") {
    return typeof action.payload === "string"
      && action.payload.startsWith("data:image/png;base64,iVBORw0KGgo")
      && action.payload.length <= 5_592_430;
  }
  return action?.type === "files"
    && Array.isArray(action.payload)
    && action.payload.length >= 1
    && action.payload.length <= 16
    && action.payload.every((path) => typeof path === "string" && path.length > 0 && path.length <= 8_192);
}

export async function onUtoolsRedirect(
  callback: (payload: UtoolsRedirectEventPayload) => void,
): Promise<UnlistenFn> {
  if (!isDesktop()) {
    return () => undefined;
  }
  return listen<UtoolsRedirectEventPayload>("ihub://utools-redirect", (event) => {
    const payload = event.payload as Partial<UtoolsRedirectEventPayload> | null;
    const candidates = Array.isArray(payload?.candidates) ? payload.candidates : null;
    if (
      typeof payload?.sourcePluginId !== "string"
      || !/^[A-Za-z0-9._-]{2,96}$/.test(payload.sourcePluginId)
      || typeof payload.label !== "string"
      || payload.label.length === 0
      || payload.label.length > 1_024
      || !candidates
      || candidates.length === 0
      || candidates.length > 32
      || candidates.some((candidate) => (
        !candidate
        || typeof candidate.pluginId !== "string"
        || typeof candidate.commandId !== "string"
        || typeof candidate.pluginName !== "string"
        || typeof candidate.commandName !== "string"
      ))
      || !isUtoolsRedirectAction(payload.action)
    ) {
      return;
    }
    callback(payload as UtoolsRedirectEventPayload);
  });
}

export async function onPluginShortcutsChanged(
  callback: () => void,
): Promise<UnlistenFn> {
  if (!isDesktop()) {
    return () => undefined;
  }
  return listen("ihub://plugin-shortcuts-changed", callback);
}

export interface PluginSearchProvidersChangedPayload {
  pluginId: string;
  providerId?: string;
  registered: boolean;
}

export async function onPluginSearchProvidersChanged(
  callback: (payload: PluginSearchProvidersChangedPayload) => void,
): Promise<UnlistenFn> {
  if (!isDesktop()) {
    return () => undefined;
  }
  return listen<PluginSearchProvidersChangedPayload>(
    "ihub://plugin-search-providers-changed",
    (event) => {
      const payload =
        event.payload as Partial<PluginSearchProvidersChangedPayload> | null;
      if (
        typeof payload?.pluginId !== "string"
        || typeof payload.registered !== "boolean"
        || (
          payload.providerId !== undefined
          && typeof payload.providerId !== "string"
        )
        || (payload.registered && typeof payload.providerId !== "string")
      ) {
        return;
      }
      callback(payload as PluginSearchProvidersChangedPayload);
    },
  );
}

export async function onSuperPanel(
  callback: (payload: SuperPanelEvent) => void,
): Promise<UnlistenFn> {
  if (!isDesktop()) {
    return () => undefined;
  }
  return listen<SuperPanelEvent>("ihub://super-panel", (event) => {
    const payload = event.payload as Partial<SuperPanelEvent> | null;
    if (
      typeof payload?.contextToken !== "string"
      || typeof payload.physicalX !== "number"
      || typeof payload.physicalY !== "number"
      || typeof payload.expiresInMs !== "number"
    ) {
      return;
    }
    callback(payload as SuperPanelEvent);
  });
}

/** Reads the native logger's bounded projection. Browser preview returns a
 * static, content-free fixture and never attempts filesystem access. */
export async function readHostLog(): Promise<HostLogSnapshot> {
  if (!isDesktop()) {
    return browserHostLogSnapshot();
  }
  return normalizeHostLogSnapshot(
    await command<HostLogSnapshot>("get_host_log"),
  );
}

/** Clears only the host logger's fixed rotating files. Browser preview clears
 * a fresh fixture in memory so UI validation cannot mutate the workstation. */
export async function clearHostLog(
  current?: HostLogSnapshot,
): Promise<HostLogSnapshot> {
  if (!isDesktop()) {
    return emptyHostLogSnapshot(current ?? browserHostLogSnapshot());
  }
  return normalizeHostLogSnapshot(
    await command<HostLogSnapshot>("clear_host_log"),
  );
}
