import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { OcrWorkbench } from "./OcrWorkbench";

describe("OcrWorkbench", () => {
  it("renders the screenshot-to-local-text workflow and privacy boundary", () => {
    const markup = renderToStaticMarkup(
      <OcrWorkbench onClose={() => undefined} onCopy={() => undefined} onToast={() => undefined} />,
    );
    expect(markup).toContain("屏幕 OCR");
    expect(markup).toContain("截取主显示器");
    expect(markup).toContain("识别选区");
    expect(markup).toContain("Windows 本地识别 · 网络请求 0");
    expect(markup).toContain("普通插件无法调用这条像素通道");
    expect(markup).toContain("浏览器开发预览不会调用 Windows OCR");
  });
});
