import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { NetworkWorkbench, speedGrade, type NetworkSpeedResult } from "./NetworkWorkbench";

const fastResult: NetworkSpeedResult = {
  latencyMs: 18,
  jitterMs: 2.4,
  downloadMbps: 128,
  uploadMbps: 32,
  downloadBytes: 10_000_000,
  uploadBytes: 5_000_000,
  durationMs: 2_400,
  provider: "Cloudflare Edge",
};

describe("NetworkWorkbench", () => {
  it("states the fixed byte and privacy boundaries", () => {
    const markup = renderToStaticMarkup(
      <NetworkWorkbench onClose={() => undefined} onCopy={() => undefined} onToast={() => undefined} />,
    );
    expect(markup).toContain("固定端点 · 用户触发");
    expect(markup).toContain("10.0 MB");
    expect(markup).toContain("5.0 MB");
    expect(markup).toContain("固定字节，无本地文件");
    expect(markup).toContain("浏览器预览不会联网");
  });

  it("grades a low-latency fast connection", () => {
    expect(speedGrade(fastResult)).toEqual({
      label: "连接优秀",
      detail: "适合高清视频、云端协作与实时通话。",
    });
  });
});
