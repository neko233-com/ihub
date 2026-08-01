import { readFileSync } from "node:fs";
import { createRef } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { PluginInfo } from "../lib/types";
import { PLUGIN_SUB_INPUT_MAX_VALUE_LENGTH } from "../lib/plugin-sub-input";
import {
  PluginFrontendFrame,
  PluginFrontendIframe,
  PluginSubInputField,
} from "./PluginFrontendFrame";

const surfacePlugin: PluginInfo = {
  id: "test.surface",
  name: "示例插件",
  version: "1.0.0",
};
const onePixelPng = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZrG8AAAAASUVORK5CYII=";

describe("PluginFrontendFrame surface chrome", () => {
  it("routes bounded uTools height requests only through the visible trusted host", () => {
    const frameSource = readFileSync(
      new URL("./PluginFrontendFrame.tsx", import.meta.url),
      "utf8",
    );
    const appSource = readFileSync(new URL("../App.tsx", import.meta.url), "utf8");

    expect(frameSource).toContain('utoolsWindowMethod === "compatibility.utools.window.setHeight" && !onSetExpendHeight');
    expect(frameSource).toContain("onSetExpendHeight?.(params.height)");
    expect(appSource).toContain("const [pluginExpendHeight, setPluginExpendHeight]");
    expect(appSource).toContain("? (pluginExpendHeight ?? 444) + 60");
    expect(appSource).toContain("onSetExpendHeight={setPluginExpendHeight}");
  });

  it("sandboxes every real plugin document without navigation or popup capabilities", () => {
    const markup = renderToStaticMarkup(
      <PluginFrontendIframe
        allowDisplayCapture={false}
        allowMicrophone={false}
        frameRef={createRef<HTMLIFrameElement>()}
        onError={() => undefined}
        onLoad={() => undefined}
        purpose="surface"
        sourceUrl="http://127.0.0.1:43123/index.html"
        title="sandbox contract"
      />,
    );

    expect(markup).toContain('sandbox="allow-scripts allow-same-origin"');
    expect(markup).toContain('referrerPolicy="no-referrer"');
    expect(markup).not.toContain("allow-top-navigation");
    expect(markup).not.toContain("allow-popups");
    expect(markup).not.toContain("allow-downloads");
    expect(markup).not.toContain("allow-forms");
    expect(markup).not.toContain("allow-modals");
  });

  it("delegates display capture only to a declared visible surface", () => {
    const renderIframe = (
      allowDisplayCapture: boolean,
      purpose: "runtime" | "surface",
    ) =>
      renderToStaticMarkup(
        <PluginFrontendIframe
          allowDisplayCapture={allowDisplayCapture}
          allowMicrophone={false}
          frameRef={createRef<HTMLIFrameElement>()}
          onError={() => undefined}
          onLoad={() => undefined}
          purpose={purpose}
          sourceUrl="http://127.0.0.1:43123/index.html"
          title={`${purpose} display capture contract`}
        />,
      );

    const declaredSurface = renderIframe(true, "surface");
    const undeclaredSurface = renderIframe(false, "surface");
    const declaredRuntime = renderIframe(true, "runtime");

    expect(declaredSurface).toContain('allow="display-capture"');
    expect(undeclaredSurface).not.toContain('allow="display-capture"');
    expect(declaredRuntime).not.toContain('allow="display-capture"');
  });

  it("delegates microphone only to a declared visible surface", () => {
    const renderIframe = (
      allowMicrophone: boolean,
      purpose: "runtime" | "surface",
    ) =>
      renderToStaticMarkup(
        <PluginFrontendIframe
          allowDisplayCapture={false}
          allowMicrophone={allowMicrophone}
          frameRef={createRef<HTMLIFrameElement>()}
          onError={() => undefined}
          onLoad={() => undefined}
          purpose={purpose}
          sourceUrl="http://127.0.0.1:43123/index.html"
          title={`${purpose} microphone contract`}
        />,
      );

    const declaredSurface = renderIframe(true, "surface");
    const undeclaredSurface = renderIframe(false, "surface");
    const declaredRuntime = renderIframe(true, "runtime");

    expect(declaredSurface).toContain('allow="microphone"');
    expect(undeclaredSurface).not.toContain('allow="microphone"');
    expect(declaredRuntime).not.toContain('allow="microphone"');
  });

  it("combines independently native-projected media delegations", () => {
    const markup = renderToStaticMarkup(
      <PluginFrontendIframe
        allowDisplayCapture
        allowMicrophone
        frameRef={createRef<HTMLIFrameElement>()}
        onError={() => undefined}
        onLoad={() => undefined}
        purpose="surface"
        sourceUrl="http://127.0.0.1:43123/index.html"
        title="combined media contract"
      />,
    );

    expect(markup).toContain('allow="display-capture; microphone"');
  });

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
    expect(markup).not.toContain("子输入框");
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

  it("shows detach only on the trusted launcher plugin surface", () => {
    const launcherMarkup = renderToStaticMarkup(
      <PluginFrontendFrame
        onClose={() => undefined}
        onDetach={() => undefined}
        onPendingEventHandled={() => undefined}
        onToast={() => undefined}
        pendingEvent={null}
        plugin={surfacePlugin}
      />,
    );
    const detachedMarkup = renderToStaticMarkup(
      <PluginFrontendFrame
        onClose={() => undefined}
        onDetach={() => undefined}
        onPendingEventHandled={() => undefined}
        onToast={() => undefined}
        pendingEvent={null}
        placement="detached"
        plugin={surfacePlugin}
      />,
    );

    expect(launcherMarkup).toContain("分离窗口");
    expect(launcherMarkup).toContain("Ctrl D");
    expect(launcherMarkup).toContain("在分离窗口中打开 示例插件");
    expect(detachedMarkup).toContain("关闭插件分离窗口");
    expect(detachedMarkup).not.toContain("在分离窗口中打开 示例插件");
    expect(detachedMarkup).not.toContain("Ctrl D");
  });

  it("renders a bounded host-owned sub-input without exposing plugin HTML controls", () => {
    const markup = renderToStaticMarkup(
      <PluginSubInputField
        inputRef={createRef<HTMLInputElement>()}
        onChange={() => undefined}
        placeholder={'搜索 <本机文件> "安全"'}
        pluginName="示例插件"
        value="needle"
      />,
    );

    expect(markup).toContain("plugin-frame__sub-input");
    expect(markup).toContain('aria-label="示例插件 子输入框"');
    expect(markup).toContain(`maxLength="${PLUGIN_SUB_INPUT_MAX_VALUE_LENGTH}"`);
    expect(markup).toContain('type="text"');
    expect(markup).toContain('value="needle"');
    expect(markup).toContain("搜索 &lt;本机文件&gt; &quot;安全&quot;");
    expect(markup).not.toContain("<iframe");
    expect(markup).not.toContain("contenteditable");
  });
});
