import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { TranslationWorkbench } from "./TranslationWorkbench";

describe("TranslationWorkbench", () => {
  it("renders a private bilingual canvas with explicit local model controls", () => {
    const markup = renderToStaticMarkup(
      <TranslationWorkbench
        input="你好"
        onClose={() => undefined}
        onCopy={() => undefined}
        onInputChange={() => undefined}
        onToast={() => undefined}
      />,
    );

    expect(markup).toContain("离线翻译");
    expect(markup).toContain("本地处理 · 网络请求 0");
    expect(markup).toContain("默认中英词典路由已随应用安装");
    expect(markup).toContain("词典覆盖 100%");
    expect(markup).toContain("hello");
    expect(markup).toContain("data-swap-parity=\"even\"");
  });

  it("keeps imported pack detection and byte-sized storage checks in the client boundary", async () => {
    const source = await import("./TranslationWorkbench?raw");
    expect(source.default).toContain("detectOfflineLanguage(input, packs)");
    expect(source.default).toContain("file.size > MAX_OFFLINE_PACK_BYTES");
    expect(source.default).toContain("serializedByteLength(serialized) > maximumStoredPackBytes");
  });
});
