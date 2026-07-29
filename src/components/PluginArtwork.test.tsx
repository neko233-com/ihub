import { Puzzle } from "lucide-react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { PluginCommandInfo, PluginInfo, SearchResult } from "../lib/types";
import {
  PluginArtwork,
  pluginCommandArtworkSrc,
  pluginSearchResultArtworkSrc,
  safePluginArtworkSrc,
} from "./PluginArtwork";

const pluginPng = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZrG8AAAAASUVORK5CYII=";
const commandPng = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAIAAAABCAQAAACRGkxeAAAAC0lEQVR42mP8/x8AAusB9Y9ZrG8AAAAASUVORK5CYII=";

const command: PluginCommandInfo = {
  execution: "frontend",
  iconSrc: commandPng,
  id: "open",
  name: "打开",
};
const plugin: PluginInfo = {
  commands: [command],
  iconSrc: pluginPng,
  id: "test.plugin",
  name: "示例插件",
  version: "1.0.0",
};

describe("plugin artwork", () => {
  it("prefers command artwork, then plugin artwork, then no raster source", () => {
    expect(pluginCommandArtworkSrc(plugin, command)).toBe(commandPng);
    expect(pluginCommandArtworkSrc(plugin, {
      iconSrc: "https://example.com/unsafe.png",
      id: "unsafe",
    })).toBe(pluginPng);
    expect(pluginCommandArtworkSrc(
      { iconSrc: "C:\\plugins\\icon.png" },
      { iconSrc: "data:image/svg+xml;base64,PHN2Zz4=", id: "unsafe" },
    )).toBeUndefined();
  });

  it("uses the same command-first identity for plugin search results", () => {
    const result: SearchResult = {
      commandId: "open",
      id: "plugin-command:test.plugin:open",
      kind: "plugin",
      name: "打开",
      pluginId: plugin.id,
      score: 900,
    };

    expect(pluginSearchResultArtworkSrc(result, [plugin])).toBe(commandPng);
    expect(pluginSearchResultArtworkSrc({ ...result, commandId: "missing" }, [plugin]))
      .toBe(pluginPng);
    expect(pluginSearchResultArtworkSrc({ ...result, kind: "application" }, [plugin]))
      .toBeUndefined();
  });

  it("renders only validated PNG data URLs and otherwise keeps the fallback markup", () => {
    const validMarkup = renderToStaticMarkup(
      <PluginArtwork
        className="plugin-artwork"
        fallback={<Puzzle data-testid="fallback" />}
        iconSrc={pluginPng}
      />,
    );
    const unsafeMarkup = renderToStaticMarkup(
      <PluginArtwork
        fallback={<Puzzle data-testid="fallback" />}
        iconSrc="https://example.com/unsafe.png"
      />,
    );

    expect(validMarkup).toContain('<img alt="" class="plugin-artwork"');
    expect(validMarkup).toContain('draggable="false"');
    expect(validMarkup).not.toContain("<svg");
    expect(unsafeMarkup).toContain("<svg");
    expect(unsafeMarkup).toContain('data-testid="fallback"');
    expect(unsafeMarkup).not.toContain("https://example.com/unsafe.png");
    expect(safePluginArtworkSrc("javascript:alert(1)")).toBeUndefined();
  });
});
