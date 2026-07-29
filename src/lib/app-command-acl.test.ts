import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const appSource = readFileSync(
  new URL("../../src-tauri/src/app.rs", import.meta.url),
  "utf8",
);
const permissionSource = readFileSync(
  new URL("../../src-tauri/permissions/app-commands.toml", import.meta.url),
  "utf8",
);
const buildSource = readFileSync(
  new URL("../../src-tauri/build.rs", import.meta.url),
  "utf8",
);
const mainCapability = JSON.parse(readFileSync(
  new URL("../../src-tauri/capabilities/default.json", import.meta.url),
  "utf8",
)) as { permissions?: unknown };
const detachedCapability = JSON.parse(readFileSync(
  new URL("../../src-tauri/capabilities/plugin-detached.json", import.meta.url),
  "utf8",
)) as { permissions?: unknown };

function handlerCommands(): string[] {
  const block = appSource.match(
    /\.invoke_handler\(tauri::generate_handler!\[\s*([\s\S]*?)\s*\]\)/,
  )?.[1];
  if (!block) {
    throw new Error("Could not find the Tauri invoke_handler command list.");
  }
  return block
    .split(",")
    .map((entry) => entry.trim().split("::").at(-1) ?? "")
    .filter(Boolean)
    .sort();
}

function permissionCommands(identifier: string): string[] {
  const escaped = identifier.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  const block = permissionSource.match(new RegExp(
    `identifier\\s*=\\s*"${escaped}"[\\s\\S]*?commands\\.allow\\s*=\\s*\\[([\\s\\S]*?)\\]`,
  ))?.[1];
  if (!block) {
    throw new Error(`Could not find application permission '${identifier}'.`);
  }
  return [...block.matchAll(/"([^"]+)"/g)].map((match) => match[1]!).sort();
}

describe("Tauri application command ACL", () => {
  it("keeps every invoke_handler command explicit for the trusted main window", () => {
    expect(buildSource).toContain(".app_manifest(tauri_build::AppManifest::new())");
    expect(permissionCommands("main-app-commands")).toEqual(handlerCommands());
    expect(mainCapability.permissions).toContain("main-app-commands");
  });

  it("gives detached hosts only their exact bootstrap and leased Bridge surface", () => {
    expect(permissionCommands("detached-plugin-host-commands")).toEqual([
      "close_detached_plugin_window",
      "get_detached_plugin_window_bootstrap",
      "get_plugin_frontend_url",
      "issue_plugin_cursor_color_approval",
      "plugin_host_call",
      "release_plugin_frontend_url",
      "touch_plugin_frontend_lease",
    ]);
    expect(detachedCapability.permissions).toContain("detached-plugin-host-commands");
    expect(detachedCapability.permissions).not.toContain("main-app-commands");
  });
});
