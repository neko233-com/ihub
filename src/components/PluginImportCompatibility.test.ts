import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";
import { inspectGitHubImportSource } from "./PluginCenter";

describe("iHub / uTools plugin import entry points", () => {
  it("accepts canonical GitHub repository, tag, commit, and fragment forms", () => {
    expect(inspectGitHubImportSource("owner/utools-plugin-demo").isPlausible).toBe(true);
    expect(inspectGitHubImportSource("github:owner/utools-plugin-demo@v1.2.3")).toMatchObject({
      isPlausible: true,
      requestedRef: "v1.2.3",
    });
    expect(inspectGitHubImportSource("owner/utools-plugin-demo@0123456789abcdef")).toMatchObject({
      isPlausible: true,
      requestedRef: "0123456789abcdef",
    });
    expect(inspectGitHubImportSource("https://github.com/owner/utools-plugin-demo.git#release")).toMatchObject({
      isPlausible: true,
      requestedRef: "release",
    });
  });

  it("rejects non-GitHub, credential-bearing, and ambiguous repository inputs", () => {
    for (const source of [
      "http://github.com/owner/plugin",
      "https://token@github.com/owner/plugin",
      "https://gitlab.com/owner/plugin",
      "owner/plugin/extra",
      "../plugin",
    ]) {
      expect(inspectGitHubImportSource(source).isPlausible, source).toBe(false);
    }
  });

  it("exposes both local and GitHub uTools import actions in Plugin Center", () => {
    const source = readFileSync(new URL("./PluginCenter.tsx", import.meta.url), "utf8");
    expect(source).toContain("从 GitHub 导入 iHub / uTools 插件");
    expect(source).toContain("接入本地 iHub / uTools 插件");
    expect(source).toContain('command<PluginInfo>("link_plugin_from_local"');
    expect(source).toContain('command<PluginInfo>("install_plugin_from_git"');
    expect(source).toContain('plugin.compatibility === "utools"');
  });
});
