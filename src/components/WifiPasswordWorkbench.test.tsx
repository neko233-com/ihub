import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { WifiPasswordWorkbench, wifiSecurityLabel } from "./WifiPasswordWorkbench";

describe("WifiPasswordWorkbench", () => {
  it("states the explicit UAC, no-fake-secret and memory boundaries", () => {
    const markup = renderToStaticMarkup(<WifiPasswordWorkbench onClose={() => undefined} onCopy={() => undefined} onToast={() => undefined} />);
    expect(markup).toContain("单项授权 · 内存显示 · 60 秒清除");
    expect(markup).toContain("每次读取都会出现 UAC");
    expect(markup).toContain("浏览器预览不枚举真实 SSID、不触发 UAC，也不展示示例密码");
    expect(markup).toContain("不调用 netsh / PowerShell");
  });

  it("formats profile security metadata without touching a credential", () => {
    expect(wifiSecurityLabel({ authentication: "WPA2PSK", encryption: "AES" })).toBe("WPA2PSK · AES");
  });
});
