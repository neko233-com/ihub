import type { PluginCommandInfo, PluginInfo } from "./types";

/**
 * The only categories a launcher may offer for an explicit plugin handoff.
 * These are not ambient permissions: a source remains in the trusted parent
 * renderer until the person confirms one concrete plugin command.
 */
export type LauncherContextCategory = "text" | "files" | "image";

export interface LauncherContextFileSource {
  path: string;
  name: string;
  kind: "file" | "folder";
}

export interface LauncherContextImageSource {
  /** Kept only in the current renderer session. It is never persisted or sent
   * through the plugin bridge; the host receives bounded metadata only. */
  blob: Blob;
  name: string;
  type: string;
}

interface LauncherContextHandoffBase {
  /** Renderer-session identity used to avoid confusing a stale confirmation
   * with a newer pasted/typed launcher source. */
  id: string;
  /** Product wording such as “翻译” or “OCR”; it is never plugin input. */
  suggestedUse: string;
}

export interface LauncherTextContextHandoff extends LauncherContextHandoffBase {
  kind: "text";
  text: string;
}

export interface LauncherFilesContextHandoff extends LauncherContextHandoffBase {
  kind: "files";
  files: readonly LauncherContextFileSource[];
}

export interface LauncherImageContextHandoff extends LauncherContextHandoffBase {
  kind: "image";
  image: LauncherContextImageSource;
}

/**
 * Sensitive source data stays in App state only until the final, visible
 * confirmation. Do not put this object in localStorage, logs, history, or a
 * plugin event.
 */
export type LauncherContextHandoff =
  | LauncherTextContextHandoff
  | LauncherFilesContextHandoff
  | LauncherImageContextHandoff;

/** Safe, category-only view for the Plugin Center confirmation UI. */
export interface LauncherContextHandoffPreview {
  id: string;
  suggestedUse: string;
  categories: readonly LauncherContextCategory[];
  title: string;
  detail: string;
}

export interface LauncherContextEligibleCommand {
  plugin: PluginInfo;
  command: PluginCommandInfo;
}

function plural(count: number, singular: string, pluralLabel: string) {
  return count === 1 ? `1 个${singular}` : `${count} 个${pluralLabel}`;
}

/** Deliberately avoids returning text contents in the view model. */
export function previewLauncherContextHandoff(
  handoff: LauncherContextHandoff,
): LauncherContextHandoffPreview {
  switch (handoff.kind) {
    case "text":
      return {
        id: handoff.id,
        suggestedUse: handoff.suggestedUse,
        categories: ["text"],
        title: "当前输入的文本",
        detail: `${handoff.text.length} 个字符；仅在确认后交给所选插件命令。`,
      };
    case "files": {
      const folders = handoff.files.filter((file) => file.kind === "folder").length;
      const files = handoff.files.length - folders;
      const labels = [
        files ? plural(files, "文件", "文件") : "",
        folders ? plural(folders, "文件夹", "文件夹") : "",
      ].filter(Boolean);
      return {
        id: handoff.id,
        suggestedUse: handoff.suggestedUse,
        categories: ["files"],
        title: "已粘贴的文件或文件夹",
        detail: `${labels.join("、") || "已选择项目"}；插件只会收到名称、类型、大小和不透明 handle，不会收到路径或读取权限；要读取内容仍须在系统选择器中重新选择。`,
      };
    }
    case "image":
      return {
        id: handoff.id,
        suggestedUse: handoff.suggestedUse,
        categories: ["image"],
        title: "已粘贴的图片",
        detail: `“${handoff.image.name || "图片"}”；插件只会收到图片元数据和不透明 handle，不会收到像素；要处理图片仍须在系统选择器中重新选择。`,
      };
  }
}

export function launcherContextCategories(
  handoff: Pick<LauncherContextHandoff, "kind"> | LauncherContextHandoffPreview,
): readonly LauncherContextCategory[] {
  return "categories" in handoff ? handoff.categories : [handoff.kind];
}

function pluginAllowsCategories(
  plugin: PluginInfo,
  categories: readonly LauncherContextCategory[],
) {
  const declared = plugin.launcherContext;
  if (!declared) {
    return false;
  }
  return categories.every((category) => declared[category] === true);
}

/**
 * The renderer uses this only to present candidates. Rust repeats the same
 * exact checks when issuing and consuming a context, so a stale plugin list
 * cannot broaden a handoff.
 */
export function eligibleLauncherContextCommands(
  plugins: readonly PluginInfo[],
  handoff: Pick<LauncherContextHandoff, "kind"> | LauncherContextHandoffPreview,
): LauncherContextEligibleCommand[] {
  const categories = launcherContextCategories(handoff);
  return plugins.flatMap((plugin) => {
    if (
      plugin.enabled === false
      || !plugin.frontendEntry
      || !Array.isArray(plugin.commands)
      || !pluginAllowsCategories(plugin, categories)
    ) {
      return [];
    }
    return plugin.commands
      .filter((command): command is PluginCommandInfo =>
        Boolean(command?.id) && command.execution === "frontend",
      )
      .map((command) => ({ plugin, command }));
  });
}

export function launcherContextCategoryLabel(category: LauncherContextCategory) {
  switch (category) {
    case "text":
      return "文本";
    case "files":
      return "文件元数据";
    case "image":
      return "图片元数据";
  }
}
