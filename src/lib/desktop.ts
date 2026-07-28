import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

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
