import { describe, expect, it } from "vitest";

import { resolvePluginShortcutTarget } from "./plugin-shortcut-settings";
import type { PluginInfo } from "./types";

const plugin: PluginInfo = {
  id: "demo.plugin",
  name: "Demo",
  version: "1",
  commands: [{
    id: "translate",
    name: "离线翻译",
    execution: "frontend",
    keywords: ["Translate", "翻译"],
  }],
};

describe("uTools shortcut settings target", () => {
  it("matches only the requesting plugin's exact command name or alias", () => {
    expect(resolvePluginShortcutTarget([plugin], {
      pluginId: "demo.plugin",
      commandLabel: " translate ",
      autoCopy: true,
    })?.command.id).toBe("translate");
    expect(resolvePluginShortcutTarget([plugin], {
      pluginId: "other.plugin",
      commandLabel: "翻译",
      autoCopy: false,
    })).toBeNull();
    expect(resolvePluginShortcutTarget([plugin], {
      pluginId: "demo.plugin",
      commandLabel: "译",
      autoCopy: false,
    })).toBeNull();
  });
});
