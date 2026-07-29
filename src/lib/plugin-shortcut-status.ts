import type { PluginInfo } from "./types";

export interface PluginShortcutStatusSummary {
  total: number;
  registered: number;
  failed: number;
  label: string;
  title?: string;
}

export function pluginShortcutStatusSummary(
  plugin?: PluginInfo,
): PluginShortcutStatusSummary | null {
  if (!plugin) {
    return null;
  }
  const bindings = [
    ...(Array.isArray(plugin.commands)
      ? plugin.commands.flatMap((command) => command.shortcut
        ? [{
          label: command.name || command.id,
          shortcut: command.shortcut,
          registration: command.shortcutRegistration,
          error: command.shortcutError,
        }]
        : [])
      : []),
    ...(plugin.globalShortcuts ?? []).map((shortcut) => ({
      label: shortcut.keyword
        ? `搜索“${shortcut.keyword}”`
        : `命令 ${shortcut.commandId ?? shortcut.id}`,
      shortcut: shortcut.shortcut,
      registration: shortcut.registration,
      error: shortcut.error,
    })),
  ];
  if (bindings.length === 0) {
    return null;
  }
  const registered = bindings.filter((binding) => binding.registration === "registered").length;
  const failed = bindings.filter((binding) =>
    !["registered", "inactive"].includes(binding.registration ?? "unavailable"),
  ).length;
  const title = bindings
    .map((binding) => [
      `${binding.shortcut} → ${binding.label}`,
      binding.registration === "registered"
        ? "已注册"
        : binding.registration === "inactive"
          ? "插件停用"
          : binding.error ?? "未注册",
    ].join("："))
    .join("\n");
  return {
    total: bindings.length,
    registered,
    failed,
    label: failed > 0
      ? `快捷键 ${registered}/${bindings.length} · ${failed} 个失败`
      : plugin.enabled === false
        ? `快捷键 ${bindings.length} 个 · 已停用`
        : `快捷键 ${registered}/${bindings.length}`,
    title,
  };
}
