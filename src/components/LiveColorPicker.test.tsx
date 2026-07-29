import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { LiveColorPicker } from "./LiveColorPicker";

describe("LiveColorPicker", () => {
  it("advertises the bounded live workflow without claiming a one-shot delay", () => {
    const markup = renderToStaticMarkup(
      <LiveColorPicker onConfirm={() => undefined} onStatus={() => undefined} />,
    );

    expect(markup).toContain('aria-label="实时 9 × 9 取色器"');
    expect(markup).toContain("启动模拟取色（开发验证）");
    expect(markup).toContain("最多 15 次/秒");
    expect(markup).toContain("左键确认并复制，右键或 Esc 取消");
    expect(markup).toContain("不注入输入");
    expect(markup).not.toContain("2 秒后");
    expect(markup).not.toContain("只读取一次");
  });
});
