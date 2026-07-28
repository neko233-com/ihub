import type { SearchResult } from "./types";

interface LauncherSystemCommand {
  commandId: string;
  name: string;
  metadata: string;
  aliases: readonly string[];
}

const systemCommands: readonly LauncherSystemCommand[] = [
  {
    commandId: "ihub.open-settings",
    name: "偏好设置",
    metadata: "自动更新、开机启动、快捷键与启动器偏好",
    aliases: ["设置", "preferences", "settings", "autostart", "update", "快捷键"],
  },
];

function normalized(value: string) {
  return value.trim().toLocaleLowerCase();
}

/** System surfaces are query-only: the home grid stays focused on daily tools. */
export function launcherSystemCommandResults(query: string): SearchResult[] {
  const needle = normalized(query);
  if (!needle) {
    return [];
  }

  return systemCommands
    .filter((command) => [command.name, command.metadata, command.commandId, ...command.aliases]
      .join(" ")
      .toLocaleLowerCase()
      .includes(needle))
    .map((command, index) => ({
      id: `system-command:${command.commandId}`,
      name: command.name,
      kind: "command" as const,
      score: 990 - index,
      metadata: `系统命令 · ${command.metadata}`,
      commandId: command.commandId,
    }));
}
