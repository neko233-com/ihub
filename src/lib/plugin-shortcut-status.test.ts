import { describe, expect, it } from "vitest";

import { pluginShortcutStatusSummary } from "./plugin-shortcut-status";
import type { PluginInfo } from "./types";

function plugin(): PluginInfo {
  return {
    id: "demo-plugin",
    name: "Demo",
    version: "1.0.0",
    enabled: true,
    commands: [{
      id: "open",
      name: "Open",
      execution: "frontend",
      shortcut: "Alt+KeyD",
      shortcutRegistration: "registered",
    }],
    globalShortcuts: [{
      id: "find",
      shortcut: "CmdOrCtrl+Alt+KeyF",
      keyword: "find",
      registration: "blocked",
      error: "快捷键冲突。",
    }],
  };
}

describe("plugin shortcut status summary", () => {
  it("makes partial native registration failures visible", () => {
    const summary = pluginShortcutStatusSummary(plugin());
    expect(summary).toMatchObject({
      total: 2,
      registered: 1,
      failed: 1,
      label: "快捷键 1/2 · 1 个失败",
    });
    expect(summary?.title).toContain("快捷键冲突");
  });

  it("does not invent a status for plugins without declarations", () => {
    const withoutShortcuts = plugin();
    withoutShortcuts.commands = [];
    withoutShortcuts.globalShortcuts = [];
    expect(pluginShortcutStatusSummary(withoutShortcuts)).toBeNull();
  });
});
