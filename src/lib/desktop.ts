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

export async function onFocusSearch(callback: () => void): Promise<UnlistenFn> {
  if (!isDesktop()) {
    return () => undefined;
  }

  return listen("ihub://focus-search", callback);
}
