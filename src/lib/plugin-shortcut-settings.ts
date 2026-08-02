import type { PluginCommandInfo, PluginInfo } from "./types";

export interface SettingsPluginShortcutTarget {
  pluginId: string;
  commandLabel: string;
  autoCopy: boolean;
}

export interface ResolvedPluginShortcutTarget {
  plugin: PluginInfo;
  command: PluginCommandInfo;
}

export function resolvePluginShortcutTarget(
  plugins: readonly PluginInfo[],
  target: SettingsPluginShortcutTarget | null,
): ResolvedPluginShortcutTarget | null {
  if (!target) return null;
  const plugin = plugins.find((candidate) => candidate.id === target.pluginId);
  if (!plugin || !Array.isArray(plugin.commands)) return null;
  const expected = target.commandLabel.trim().toLocaleLowerCase();
  const command = plugin.commands.find((candidate) =>
    candidate.name.trim().toLocaleLowerCase() === expected
    || candidate.keywords?.some((keyword) => keyword.trim().toLocaleLowerCase() === expected),
  );
  return command ? { plugin, command } : null;
}
