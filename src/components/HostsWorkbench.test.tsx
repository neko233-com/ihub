import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { HostsWorkbench, hostsFingerprintLabel, normalizeHostsEntries } from "./HostsWorkbench";

describe("HostsWorkbench", () => {
  it("states the preview, fingerprint, backup and browser boundaries", () => {
    const markup = renderToStaticMarkup(<HostsWorkbench onClose={() => undefined} onToast={() => undefined} />);
    expect(markup).toContain("固定系统路径 · 指纹校验 · 原子备份");
    expect(markup).toContain("只编辑带 iHub 标记的区块");
    expect(markup).toContain("预览并应用更改");
    expect(markup).toContain("浏览器预览不读取或写入 Windows hosts");
  });

  it("normalizes renderer rows without pretending to validate native rules", () => {
    expect(normalizeHostsEntries([{ id: "x", ip: " 127.0.0.1 ", domains: "a.test, b.test", comment: " local ", enabled: true }])).toEqual([
      { ip: "127.0.0.1", domains: ["a.test", "b.test"], comment: "local", enabled: true },
    ]);
    expect(hostsFingerprintLabel("a".repeat(64))).toBe("aaaaaaaa…aaaaaa");
  });
});
