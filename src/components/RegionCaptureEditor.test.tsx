import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { RegionCaptureEditor } from "./RegionCaptureEditor";
import { createRegionCaptureDemoSource } from "../lib/region-capture";

describe("RegionCaptureEditor", () => {
  it("exposes a real pointer selection surface with explicit crop and cancel controls", () => {
    const markup = renderToStaticMarkup(
      <RegionCaptureEditor
        developmentPreview
        onCancel={() => undefined}
        onExport={() => undefined}
        source={createRegionCaptureDemoSource()}
      />,
    );

    expect(markup).toContain('aria-label="矩形截图选区"');
    expect(markup).toContain('aria-label="截图选区画面"');
    expect(markup).toContain('role="application"');
    expect(markup).toContain("左键拖拽 · Esc 或右键取消");
    expect(markup).toContain("导出选区 PNG");
    expect(markup).toContain("创建模拟选区（开发验证）");
    expect(markup).toContain("取消");
    expect(markup).not.toContain("选择区域（原生）");
  });
});
