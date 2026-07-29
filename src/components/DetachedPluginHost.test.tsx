import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { DetachedPluginHost, DetachedPluginRouteError } from "./DetachedPluginHost";

describe("DetachedPluginHost browser QA", () => {
  it("renders host chrome and explicit no-permission status without an iframe", () => {
    const markup = renderToStaticMarkup(
      <DetachedPluginHost
        route={{
          kind: "detached",
          pluginId: "browser.preview",
          browserPreview: true,
        }}
      />,
    );

    expect(markup).toContain("plugin-frame-overlay is-detached");
    expect(markup).toContain("插件分离窗口 · 安全预览");
    expect(markup).toContain("未创建原生窗口");
    expect(markup).toContain("未签发 loopback 租约");
    expect(markup).toContain("未授予 Tauri、Node 或 shell 权限");
    expect(markup).toContain("关闭插件分离窗口");
    expect(markup).not.toContain("<iframe");
    expect(markup).not.toContain("http://127.0.0.1");
  });

  it("explains that malformed routes were rejected by the trusted host", () => {
    const markup = renderToStaticMarkup(
      <DetachedPluginRouteError message="插件标识无效。" />,
    );
    expect(markup).toContain("已拒绝分离窗口地址");
    expect(markup).toContain("插件标识无效。");
    expect(markup).toContain("固定本地地址");
  });
});
