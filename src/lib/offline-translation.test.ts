import { describe, expect, it } from "vitest";
import {
  detectOfflineLanguage,
  parseOfflineTranslationPack,
  translateOffline,
} from "./offline-translation";

describe("offline translation", () => {
  it("detects and translates the bundled Chinese-English directions", () => {
    expect(detectOfflineLanguage("本地离线翻译")).toBe("zh-CN");
    expect(detectOfflineLanguage("local offline translation")).toBe("en");
    expect(translateOffline("你好，世界", "auto", "en")).toMatchObject({
      coverage: 1,
      detectedSource: "zh-CN",
      text: "hello, world",
    });
    expect(translateOffline("good morning", "en", "zh-CN")).toMatchObject({
      coverage: 1,
      text: "早上好",
    });
  });

  it("translates known technical phrases locally and reports uncovered text", () => {
    const result = translateOffline("打开本地搜索，然后复制结果。", "zh-CN", "en");
    expect(result.text).toBe("Open local search, then copy result.");
    expect(result.coverage).toBe(1);

    const partial = translateOffline("你好量子猫", "zh-CN", "en");
    expect(partial.text).toContain("Hello");
    expect(partial.coverage).toBeLessThan(1);
    expect(partial.unknownSegments).toEqual(["量", "子", "猫"]);
  });

  it("imports bounded local dictionary packs and uses them in both directions", () => {
    const pack = parseOfflineTranslationPack(JSON.stringify({
      id: "ja-en-demo",
      name: "Japanese English demo",
      source: "ja",
      target: "en",
      version: 1,
      entries: { "こんにちは": "hello" },
    }));
    expect(translateOffline("こんにちは", "ja", "en", [pack]).text).toBe("hello");
    expect(translateOffline("hello", "en", "ja", [pack]).text).toBe("こんにちは");
  });

  it("rejects malformed, oversized, or control-bearing packs", () => {
    expect(() => parseOfflineTranslationPack("[]")).toThrow();
    expect(() => parseOfflineTranslationPack(JSON.stringify({
      id: "bad",
      name: "bad",
      source: "en",
      target: "en",
      version: 1,
      entries: { hello: "你好" },
    }))).toThrow();
    expect(() => parseOfflineTranslationPack(JSON.stringify({
      id: "bad-control",
      name: "bad",
      source: "ja",
      target: "en",
      version: 1,
      entries: { "こ\u0000": "hello" },
    }))).toThrow();
  });
});
