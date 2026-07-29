import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { PluginInfo } from "../lib/types";
import { PluginFrontendFrame } from "./PluginFrontendFrame";

const surfacePlugin: PluginInfo = {
  id: "test.surface",
  name: "示例插件",
  version: "1.0.0",
};
const onePixelPng = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZrG8AAAAASUVORK5CYII=";

describe("PluginFrontendFrame surface chrome", () => {
  it("renders the compact host bar without technical bridge footer copy", () => {
    const markup = renderToStaticMarkup(
      <PluginFrontendFrame
        onClose={() => undefined}
        onPendingEventHandled={() => undefined}
        onToast={() => undefined}
        pendingEvent={null}
        plugin={surfacePlugin}
      />,
    );

    expect(markup).toContain("plugin-frame-overlay");
    expect(markup).toContain("plugin-frame__header");
    expect(markup).toContain("返回 iHub 启动器");
    expect(markup).toContain("示例插件");
    expect(markup).toContain("安全状态：插件界面已隔离加载");
    expect(markup).not.toContain("PLUGIN FRONTEND");
    expect(markup).not.toContain("iHub Bridge");
    expect(markup).not.toContain("frontend entry");
    expect(markup).not.toContain("<footer");
  });

  it("uses validated plugin artwork in the host identity tag", () => {
    const artworkMarkup = renderToStaticMarkup(
      <PluginFrontendFrame
        onClose={() => undefined}
        onPendingEventHandled={() => undefined}
        onToast={() => undefined}
        pendingEvent={null}
        plugin={{ ...surfacePlugin, iconSrc: onePixelPng }}
      />,
    );
    const unsafeMarkup = renderToStaticMarkup(
      <PluginFrontendFrame
        onClose={() => undefined}
        onPendingEventHandled={() => undefined}
        onToast={() => undefined}
        pendingEvent={null}
        plugin={{ ...surfacePlugin, iconSrc: "https://example.com/plugin.png" }}
      />,
    );

    expect(artworkMarkup).toContain('plugin-frame__tag-icon is-artwork');
    expect(artworkMarkup).toContain(`<img alt="" draggable="false" src="${onePixelPng}"/>`);
    expect(unsafeMarkup).not.toContain("plugin-frame__tag-icon is-artwork");
    expect(unsafeMarkup).not.toContain("https://example.com/plugin.png");
  });
});
