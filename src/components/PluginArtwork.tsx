import type { ReactNode } from "react";
import { safeNativeIconSrc } from "../lib/native-icons";
import type { PluginCommandInfo, PluginInfo, SearchResult } from "../lib/types";

type PluginArtworkOwner = Pick<PluginInfo, "iconSrc">;
type PluginCommandArtworkOwner = Pick<PluginCommandInfo, "id" | "iconSrc">;

export interface PluginArtworkProps {
  className?: string;
  fallback: ReactNode;
  iconSrc?: unknown;
}

/**
 * Plugin package paths and arbitrary web URLs must never become renderer image
 * sources. The native host emits PNG data URLs, and this remains a defensive
 * renderer boundary for stale or browser-preview data.
 */
export function safePluginArtworkSrc(...candidates: readonly unknown[]): string | undefined {
  for (const candidate of candidates) {
    const iconSrc = safeNativeIconSrc(candidate);
    if (iconSrc) {
      return iconSrc;
    }
  }
  return undefined;
}

/** A command identity wins over its owning plugin, matching uTools launchers. */
export function pluginCommandArtworkSrc(
  plugin: PluginArtworkOwner,
  command?: PluginCommandArtworkOwner,
): string | undefined {
  return safePluginArtworkSrc(command?.iconSrc, plugin.iconSrc);
}

/**
 * Search-provider and command results share the exact artwork resolver used by
 * the launcher home, so the same plugin never changes identity between views.
 */
export function pluginSearchResultArtworkSrc(
  result: Pick<SearchResult, "kind" | "pluginId" | "commandId">,
  plugins: readonly PluginInfo[],
): string | undefined {
  if (result.kind !== "plugin" || !result.pluginId) {
    return undefined;
  }
  const plugin = plugins.find((candidate) => candidate.id === result.pluginId);
  if (!plugin) {
    return undefined;
  }
  const command = Array.isArray(plugin.commands) && result.commandId
    ? plugin.commands.find((candidate) => candidate.id === result.commandId)
    : undefined;
  return pluginCommandArtworkSrc(plugin, command);
}

export function PluginArtwork({
  className,
  fallback,
  iconSrc,
}: PluginArtworkProps) {
  const safeIconSrc = safePluginArtworkSrc(iconSrc);
  return safeIconSrc ? (
    <img
      alt=""
      className={className}
      draggable={false}
      src={safeIconSrc}
    />
  ) : (
    <>{fallback}</>
  );
}
