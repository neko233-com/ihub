import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const appSource = readFileSync(new URL("../App.tsx", import.meta.url), "utf8");
const toolboxSource = readFileSync(
  new URL("./ToolboxDrawer.tsx", import.meta.url),
  "utf8",
);
const localSearchSource = readFileSync(
  new URL("./LocalSearchWorkspace.tsx", import.meta.url),
  "utf8",
);
const typesSource = readFileSync(new URL("../lib/types.ts", import.meta.url), "utf8");
const hostSource = readFileSync(
  new URL("../../src-tauri/src/app.rs", import.meta.url),
  "utf8",
);
const builtinToolsSource = readFileSync(
  new URL("../../src-tauri/src/builtin_tools.rs", import.meta.url),
  "utf8",
);
const pluginRuntimeSource = readFileSync(
  new URL("../../plugin-sdk/src/runtime.ts", import.meta.url),
  "utf8",
);
const pluginTypesSource = readFileSync(
  new URL("../../plugin-sdk/src/types.ts", import.meta.url),
  "utf8",
);
const permissionSource = readFileSync(
  new URL("../../src-tauri/permissions/app-commands.toml", import.meta.url),
  "utf8",
);

describe("first-party filesystem open authorization", () => {
  it("opens pasted and newly created targets only by opaque openId", () => {
    expect(appSource).toContain(
      'command<void>("open_granted_path", { openId: result.openId })',
    );
    expect(appSource).toContain("openId: file.openId");
    expect(toolboxSource).toContain(
      'command<void>("open_granted_path", { openId })',
    );
    expect(typesSource).toMatch(
      /interface ClipboardFile \{[\s\S]*?openId: string;/,
    );
  });

  it("does not expose the generic renderer-path opener", () => {
    expect(appSource).not.toMatch(/command(?:<[^>]+>)?\("open_path"/);
    expect(toolboxSource).not.toMatch(/command(?:<[^>]+>)?\("open_path"/);
    expect(permissionSource).not.toMatch(/"open_path"/);
    expect(permissionSource).toMatch(/"open_granted_path"/);
    expect(hostSource).not.toMatch(/\n\s*pub async fn open_path\(/);
    expect(hostSource).not.toMatch(/\n\s*open_path,\s*\n/);
    expect(hostSource).toMatch(/\n\s*open_granted_path,\s*\n/);
  });

  it("uses one bounded host store for clipboard, Super Panel, and projects", () => {
    expect(hostSource).toMatch(
      /read_clipboard_files\(state: State<'_, AppState>\)[\s\S]*?clipboard_files_from_paths\(\s*&state\.temporary_path_opens,/,
    );
    expect(hostSource).toMatch(
      /consume_super_panel_context\([\s\S]*?clipboard_files_from_paths\(&state\.temporary_path_opens, paths\)/,
    );
    expect(hostSource).toMatch(
      /create_plugin_project_with_open_grant\(\s*&temporary_path_opens,/,
    );
    expect(hostSource).toContain("MAX_TEMPORARY_PATH_OPEN_GRANTS");
    expect(hostSource).toContain("TEMPORARY_PATH_OPEN_TTL");
  });

  it("binds every first-party directory capability to a current native folder grant", () => {
    expect(typesSource).toMatch(
      /interface SelectedDirectoryGrant \{[\s\S]*?path: string;[\s\S]*?openId: string;/,
    );
    expect(hostSource).toMatch(
      /pub fn select_directory\([\s\S]*?Result<Option<SelectedDirectoryGrant>, String>[\s\S]*?temporary_path_opens[\s\S]*?\.issue\(Path::new\(&directory\)\)/,
    );
    expect(hostSource).toMatch(
      /pub fn set_index_roots\([\s\S]*?directory_open_ids: Vec<String>[\s\S]*?authorize_index_root_update\(/,
    );
    expect(localSearchSource).toContain(
      'command<SelectedDirectoryGrant | null>("select_directory")',
    );
    expect(localSearchSource).toContain("await onSetIndexRoots(roots, directoryOpenIds)");
    expect(localSearchSource).toMatch(
      /aria-label="索引目录只能通过系统文件夹选择器添加"[\s\S]*?readOnly/,
    );

    expect(toolboxSource).toContain("directoryOpenId: renameDirectoryOpenId");
    expect(toolboxSource).toContain(
      "parentDirectoryOpenId: projectParentDirectoryOpenId",
    );
    expect(toolboxSource).toContain(
      "directoryOpenId: localPluginDirectoryOpenId",
    );
    expect(toolboxSource).not.toMatch(
      /command<BatchRenamePreview>\("preview_batch_rename", \{\s*directory:/,
    );
    expect(toolboxSource).not.toMatch(
      /command<PluginProjectResult>\("create_plugin_project", \{\s*parentDirectory,/,
    );
    expect(toolboxSource).not.toMatch(
      /command<PluginInfo>\("link_plugin_from_local", \{\s*directory\s*\}/,
    );
  });

  it("keeps batch rename behind app wrappers instead of direct builtin commands", () => {
    expect(hostSource).toMatch(
      /pub fn preview_batch_rename\([\s\S]*?directory_open_id: String[\s\S]*?prepare_folder\(&directory_open_id\)/,
    );
    expect(hostSource).toMatch(
      /pub fn apply_batch_rename\([\s\S]*?directory_open_id: String[\s\S]*?prepare_folder\(&directory_open_id\)/,
    );
    expect(hostSource).not.toContain("crate::builtin_tools::preview_batch_rename,\n");
    expect(hostSource).not.toContain("crate::builtin_tools::apply_batch_rename,\n");
    expect(builtinToolsSource).not.toMatch(
      /#\[tauri::command\]\s*pub fn preview_batch_rename/,
    );
    expect(builtinToolsSource).not.toMatch(
      /#\[tauri::command\]\s*pub fn apply_batch_rename/,
    );
    expect(permissionSource).toMatch(/"preview_batch_rename"/);
    expect(permissionSource).toMatch(/"apply_batch_rename"/);
  });

  it("lets plugin shell.openPath resolve only the plugin-owned folder grant", () => {
    const shellBlock = hostSource.match(
      /"shell\.openPath" \| "shell\.open" => \{([\s\S]*?)\n\s*\}\n\s*"shell\.openExternal"/,
    )?.[1];
    expect(shellBlock).toBeDefined();
    expect(shellBlock).toContain('"grantId"');
    expect(shellBlock).toContain("prepare_directory_for_grant");
    expect(shellBlock).toContain("prepared.launch()");
    expect(shellBlock).not.toContain('"path"');
    expect(shellBlock).not.toContain(".canonicalize()");
    expect(pluginRuntimeSource).toContain(
      'openPath: (grantId) => this.call("shell.openPath", { grantId })',
    );
    expect(pluginRuntimeSource).not.toMatch(
      /openPath: \(path\) => this\.call\("shell\.openPath", \{ path \}\)/,
    );
    expect(pluginTypesSource).toMatch(
      /interface PluginShell \{[\s\S]*?openPath\(grantId: string\): Promise<void>;/,
    );
  });
});
