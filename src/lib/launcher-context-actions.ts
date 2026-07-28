import { eligibleLauncherContextCommands } from "./plugin-launcher-context";
import type { PluginInfo } from "./types";

/**
 * uTools-style context actions are deliberately derived in the renderer from
 * content the person has just placed in the launcher. They are suggestions,
 * not ambient file-system permissions: a plugin suggestion opens a dedicated
 * Plugin Center handoff view and still requires a second, visible confirmation
 * for one compatible frontend command.
 */

export type LauncherContextActionId =
  | "ihub.context.json"
  | "ihub.context.local-search"
  | "ihub.context.batch-rename"
  | "ihub.context.screenshot"
  | "ihub.context.translate"
  | "ihub.context.text-tools"
  | "ihub.context.ocr"
  | "ihub.context.image-tools";

export interface LauncherContextFile {
  kind: "file" | "folder";
  name: string;
  path: string;
}

export interface LauncherContextAction {
  id: LauncherContextActionId;
  label: string;
  detail: string;
  target:
    | {
      kind: "builtin";
      commandId:
        | "ihub.tool.json"
        | "ihub.tool.local-search"
        | "ihub.tool.batch-rename"
        | "ihub.tool.screenshot";
      jsonInput?: string;
      renameDirectory?: string;
    }
    | {
      kind: "plugin-handoff";
      category: "text" | "files" | "image";
      suggestedUse: "Translate" | "Text Tools" | "OCR" | "Image Tools";
      /** First-party route used to keep the built-in suggestion honest.
       * Plugin Center may still offer any other compatible installed command. */
      preferredPluginId:
        | "ihub-plugin-translate"
        | "ihub-plugin-text-tools"
        | "ihub-plugin-ocr"
        | "ihub-plugin-image-tools";
      preferredCommandId:
        | "translate-launcher-text"
        | "process-launcher-text"
        | "recognize-launcher-image"
        | "open-image-tools";
    };
}

export interface LauncherContextActionInput {
  /** Current launcher text. It is never sent to a plugin by this module. */
  query: string;
  /** Paths come only from the native clipboard file list for this session. */
  pastedFiles?: readonly LauncherContextFile[];
  /** A bitmap is held in memory only after an explicit image paste. */
  hasPastedImage?: boolean;
  /** Host clipboard images are normalized to PNG; DOM paste may retain its
   * original MIME type and must not advertise an unsupported handoff. */
  pastedImageType?: string;
}

const MAX_JSON_CONTEXT_CHARACTERS = 256 * 1024;
const ocrImagePathExtension = /\.(?:jpe?g|png)$/i;
const imageToolsPathExtension = /\.(?:jpe?g|png|webp)$/i;
const absoluteWindowsPath = /^(?:[a-z]:[\\/]|\\\\[^\\/]+[\\/][^\\/]+)/i;

function isAbsolutePath(value: string) {
  return absoluteWindowsPath.test(value) || value.startsWith("/") || value.startsWith("~/");
}

function isJsonDocument(value: string) {
  if (
    value.length > MAX_JSON_CONTEXT_CHARACTERS
    || !(value.startsWith("{") || value.startsWith("["))
  ) {
    return false;
  }
  try {
    JSON.parse(value);
    return true;
  } catch {
    return false;
  }
}

function looksLikeTextContext(value: string) {
  // A short token such as "json" is normally a command search, not a text
  // object. Newlines and a modest amount of prose are clear enough signals
  // without tracking arbitrary clipboard contents in persistent state.
  return value.length >= 24 || /[\r\n\t]/.test(value);
}

function shortPath(path: string) {
  return path.length <= 56 ? path : `…${path.slice(-55)}`;
}

function pushOnce(actions: LauncherContextAction[], action: LauncherContextAction) {
  if (!actions.some((candidate) => candidate.id === action.id)) {
    actions.push(action);
  }
}

function pushPluginHandoff(
  actions: LauncherContextAction[],
  id: Extract<LauncherContextActionId, "ihub.context.translate" | "ihub.context.text-tools" | "ihub.context.ocr" | "ihub.context.image-tools">,
  label: string,
  category: "text" | "files" | "image",
  suggestedUse: "Translate" | "Text Tools" | "OCR" | "Image Tools",
  detail: string,
) {
  const preferred = {
    "ihub.context.translate": {
      preferredPluginId: "ihub-plugin-translate",
      preferredCommandId: "translate-launcher-text",
    },
    "ihub.context.text-tools": {
      preferredPluginId: "ihub-plugin-text-tools",
      preferredCommandId: "process-launcher-text",
    },
    "ihub.context.ocr": {
      preferredPluginId: "ihub-plugin-ocr",
      preferredCommandId: "recognize-launcher-image",
    },
    "ihub.context.image-tools": {
      preferredPluginId: "ihub-plugin-image-tools",
      preferredCommandId: "open-image-tools",
    },
  } as const;
  pushOnce(actions, {
    id,
    label,
    detail,
    target: { kind: "plugin-handoff", category, suggestedUse, ...preferred[id] },
  });
}

/**
 * Builds only actions the host can actually route today. An external-plugin
 * action is deliberately only the first step: no content is staged here.
 * The Plugin Center must present compatible commands and collect a second,
 * explicit confirmation before the trusted parent calls the host handoff API.
 */
export function deriveLauncherContextActions({
  query,
  pastedFiles = [],
  hasPastedImage = false,
  pastedImageType,
}: LauncherContextActionInput): LauncherContextAction[] {
  const actions: LauncherContextAction[] = [];
  const text = query.trim();
  const primaryFile = pastedFiles.find((file) => file.path.trim().length > 0);
  const ocrImageFile = pastedFiles.find((file) =>
    file.kind === "file" && ocrImagePathExtension.test(file.path),
  );
  const imageToolsFile = pastedFiles.find((file) =>
    file.kind === "file" && imageToolsPathExtension.test(file.path),
  );

  if (hasPastedImage) {
    pushOnce(actions, {
      id: "ihub.context.screenshot",
      label: "查看已粘贴图片",
      detail: "截图工具 · 仅在当前会话预览，不自动保存",
      target: { kind: "builtin", commandId: "ihub.tool.screenshot" },
    });
    if (pastedImageType?.toLocaleLowerCase().split(";", 1)[0] === "image/png") {
      pushPluginHandoff(
        actions,
        "ihub.context.ocr",
        "用插件识别图片文字",
        "image",
        "OCR",
        "确认后仅交付图片元数据；仍需在系统选择器中重新选择图片，插件才能取得像素",
      );
      pushPluginHandoff(
        actions,
        "ihub.context.image-tools",
        "用插件处理图片",
        "image",
        "Image Tools",
        "确认后仅交付图片元数据；仍需在系统选择器中重新选择图片，插件才能取得像素",
      );
    }
  }

  if (primaryFile) {
    pushOnce(actions, {
      id: "ihub.context.local-search",
      label: "本地搜索",
      detail: `查看索引范围 · ${shortPath(primaryFile.path)}`,
      target: { kind: "builtin", commandId: "ihub.tool.local-search" },
    });

    if (primaryFile.kind === "folder") {
      pushOnce(actions, {
        id: "ihub.context.batch-rename",
        label: "批量重命名此文件夹",
        detail: `预填路径后仍需预览和确认 · ${shortPath(primaryFile.path)}`,
        target: {
          kind: "builtin",
          commandId: "ihub.tool.batch-rename",
          renameDirectory: primaryFile.path,
        },
      });
    }

    if (ocrImageFile) {
      pushPluginHandoff(
        actions,
        "ihub.context.ocr",
        "用插件识别文件文字",
        "files",
        "OCR",
        "确认后仅交付文件元数据；仍需在系统选择器中重新选择图片，插件才能取得路径或内容",
      );
    }
    if (imageToolsFile) {
      pushPluginHandoff(
        actions,
        "ihub.context.image-tools",
        "用插件处理图片文件",
        "files",
        "Image Tools",
        "确认后仅交付文件元数据；仍需在系统选择器中重新选择图片，插件才能取得路径或内容",
      );
    }
  } else if (text && isAbsolutePath(text)) {
    // Typed paths are not trusted as open-path targets. Retain them in the
    // launcher and lead only to the existing local-search configuration.
    pushOnce(actions, {
      id: "ihub.context.local-search",
      label: "本地搜索此路径",
      detail: "打开索引工具；不会直接打开或执行该路径",
      target: { kind: "builtin", commandId: "ihub.tool.local-search" },
    });
  }

  if (text && isJsonDocument(text)) {
    pushOnce(actions, {
      id: "ihub.context.json",
      label: "JSON 格式化与校验",
      detail: "将当前输入预填到本地 JSON 工具；不会上传",
      target: {
        kind: "builtin",
        commandId: "ihub.tool.json",
        jsonInput: text,
      },
    });
  } else if (text && looksLikeTextContext(text)) {
    pushPluginHandoff(
      actions,
      "ihub.context.translate",
      "用插件翻译文本",
      "text",
      "Translate",
      "选择已声明文本上下文的插件命令；确认后才交付当前输入",
    );
    pushPluginHandoff(
      actions,
      "ihub.context.text-tools",
      "用插件处理文本",
      "text",
      "Text Tools",
      "选择已声明文本上下文的插件命令；确认后才交付当前输入",
    );
  }

  return actions;
}

/**
 * A plugin suggestion is visible only when the currently installed source
 * really exposes the exact preferred frontend command and category. This
 * keeps packaged builds honest when their immutable official tag predates a
 * newer capability available in the development checkout.
 */
export function availableLauncherContextActions(
  actions: readonly LauncherContextAction[],
  plugins: readonly PluginInfo[],
): LauncherContextAction[] {
  return actions.filter((action) => {
    const target = action.target;
    if (target.kind === "builtin") {
      return true;
    }
    return eligibleLauncherContextCommands(plugins, { kind: target.category })
      .some(({ plugin, command }) =>
        plugin.id === target.preferredPluginId
        && command.id === target.preferredCommandId,
      );
  });
}
