import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { LanShareWorkbench, formatLanShareBytes, lanShareRemainingCopy } from "./LanShareWorkbench";

describe("LanShareWorkbench", () => {
  it("states the picker, LAN, expiry and no-upload boundaries", () => {
    const markup = renderToStaticMarkup(<LanShareWorkbench onClose={() => undefined} onCopy={() => undefined} onToast={() => undefined} />);
    expect(markup).toContain("选择并分享文件");
    expect(markup).toContain("随机链接 · 仅局域网 · 无广告");
    expect(markup).toContain("30 分钟后失效");
    expect(markup).toContain("不接受上传、目录路径、任意 URL");
    expect(markup).toContain("浏览器预览不会打开端口或读取本机文件");
  });

  it("formats sizes and countdowns deterministically", () => {
    expect(formatLanShareBytes(1_048_576)).toBe("1.0 MiB");
    expect(lanShareRemainingCopy(125)).toBe("02:05");
  });
});
