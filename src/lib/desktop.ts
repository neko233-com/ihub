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
  section: "preferences" | "about";
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
        section: payload.section === "about" ? "about" : "preferences",
      });
    }
  });
}

export interface PluginGlobalShortcutEventPayload {
  pluginId: string;
  shortcut: string;
  commandId?: string;
  keyword?: string;
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
    ) {
      return;
    }
    callback(payload as PluginGlobalShortcutEventPayload);
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
