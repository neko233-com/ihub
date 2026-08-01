import { describe, expect, it } from "vitest";
import {
  buildInstalledRailEntries,
  pluginCatalog,
  preferredPluginAcquisition,
  type BuiltinToolId,
} from "./plugin-catalog";

const expectedBuiltinTools = [
  "search",
  "color",
  "screenshot",
  "clipboard",
  "json",
  "translate",
  "markdown",
  "note",
  "convert",
  "calculator",
  "time",
  "qrcode",
  "cloud",
  "record",
  "network",
  "ocr",
  "share",
  "hosts",
  "rename",
  "developer",
] as const satisfies readonly BuiltinToolId[];

describe("pluginCatalog built-in tools", () => {
  it("keeps every Spotlight built-in discoverable in Plugin Center", () => {
    const catalogTools = pluginCatalog
      .filter((entry) => entry.distribution === "builtin")
      .map((entry) => entry.builtinTool)
      .filter((tool): tool is BuiltinToolId => Boolean(tool));

    expect([...new Set(catalogTools)].sort()).toEqual([...expectedBuiltinTools].sort());
    expect(catalogTools).toHaveLength(expectedBuiltinTools.length);
  });
});

describe("pluginCatalog source-checkout tools", () => {
  it("exposes every official workspace project through the trusted local-link path", () => {
    const workspaceEntries = pluginCatalog.filter((entry) => entry.workspaceProject);

    expect(workspaceEntries.map((entry) => entry.id).sort()).toEqual([
      "ihub-plugin-archive-tools",
      "ihub-plugin-base-converter",
      "ihub-plugin-batch-rename",
      "ihub-plugin-clipboard",
      "ihub-plugin-colorpick",
      "ihub-plugin-developer-tools",
      "ihub-plugin-image-tools",
      "ihub-plugin-json-tools",
      "ihub-plugin-ocr",
      "ihub-plugin-pdf-tools",
      "ihub-plugin-qrcode",
      "ihub-plugin-quick-note",
      "ihub-plugin-screen-record",
      "ihub-plugin-screenshot",
      "ihub-plugin-text-tools",
      "ihub-plugin-translate",
      "ihub-plugin-web-actions",
      "ihub-plugin-window-manager",
    ]);
    expect(workspaceEntries.every((entry) => entry.builtinTool === undefined)).toBe(true);
    expect(
      workspaceEntries.every(
        (entry) => entry.distribution === "installable" && Boolean(entry.source),
      ),
    ).toBe(true);
  });

  it("prefers the checkout in development and keeps immutable Git fallback elsewhere", () => {
    const installable = pluginCatalog.find((entry) => entry.id === "ihub-plugin-translate")!;
    const newlyPublished = pluginCatalog.find((entry) => entry.id === "ihub-plugin-pdf-tools")!;
    const builtin = pluginCatalog.find((entry) => entry.id === "ihub-local-search")!;

    expect(preferredPluginAcquisition(installable, true)).toBe("workspace");
    expect(preferredPluginAcquisition(installable, false)).toBe("remote");
    expect(preferredPluginAcquisition(newlyPublished, true)).toBe("workspace");
    expect(preferredPluginAcquisition(newlyPublished, false)).toBe("remote");
    expect(preferredPluginAcquisition(builtin, true)).toBe("builtin");
  });
});

describe("buildInstalledRailEntries", () => {
  it("keeps every installed plugin and built-in without truncating the rail", () => {
    const installed = Array.from({ length: 18 }, (_, index) => ({
      id: `community-plugin-${index}`,
      name: `Community ${index}`,
      enabled: index % 2 === 0,
    }));

    const rail = buildInstalledRailEntries(installed);
    const builtinCount = pluginCatalog.filter((entry) => entry.distribution === "builtin").length;

    expect(rail).toHaveLength(installed.length + builtinCount);
    expect(rail.length).toBeGreaterThan(12);
    expect(rail.slice(0, installed.length).map((item) => item.installed?.id)).toEqual(
      installed.map((plugin) => plugin.id),
    );
  });

  it("deduplicates aliases while preserving host and catalog order", () => {
    const disabledAlias = {
      id: "io.ihub.translate",
      name: "翻译",
      enabled: false,
    };
    const installed = [
      { id: "community-first", name: "First", enabled: true },
      disabledAlias,
      { id: "ihub-plugin-translate", name: "Duplicate canonical", enabled: true },
      { id: "community-first", name: "Duplicate external", enabled: true },
    ];

    const rail = buildInstalledRailEntries(installed);
    const builtinIds = pluginCatalog
      .filter((entry) => entry.distribution === "builtin")
      .map((entry) => entry.id);

    expect(rail.map((item) => item.entry.id)).toEqual([
      "community-first",
      "ihub-plugin-translate",
      ...builtinIds,
    ]);
    expect(rail[1].installed).toBe(disabledAlias);
    expect(rail[1].installed?.enabled).toBe(false);
  });
});
