import { describe, expect, it } from "vitest";
import {
  detectOfflineLanguage,
  MAX_OFFLINE_PACK_BYTES,
  parseOfflineTranslationPack,
  translateOffline,
  type OfflineTranslationPack,
} from "./offline-translation";

const pack = (
  id: string,
  source: string,
  target: string,
  entries: Record<string, string>,
): OfflineTranslationPack => ({ id, name: id, source, target, version: 1, entries });

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

  it("lets custom terminology override the bundled pack while retaining fallback entries", () => {
    const terminology = pack("team-zh-en", "zh-CN", "en", {
      "你好": "greetings",
    });
    const overridden = translateOffline("你好", "zh-CN", "en", [terminology]);
    expect(overridden.text).toBe("greetings");
    expect(overridden.packId).toBe("team-zh-en");
    expect(overridden.packIds).toEqual(["team-zh-en", "builtin-zh-en-v1"]);
    expect(translateOffline("谢谢你", "zh-CN", "en", [terminology]).text).toBe("thank you");
  });

  it("detects installed scripts and routes unsupported pairs through English", () => {
    const japanese = pack("ja-en", "ja", "en", { "こんにちは": "hello" });
    const korean = pack("ko-en", "ko", "en", { "안녕하세요": "hello" });
    const french = pack("en-fr", "en", "fr", { hello: "bonjour" });

    expect(detectOfflineLanguage("こんにちは", [japanese, korean])).toBe("ja");
    expect(detectOfflineLanguage("안녕하세요", [japanese, korean])).toBe("ko");
    expect(translateOffline("こんにちは", "auto", "fr", [japanese, french])).toMatchObject({
      coverage: 1,
      detectedSource: "ja",
      packIds: ["ja-en", "en-fr"],
      pivotLanguage: "en",
      text: "bonjour",
    });
  });

  it("returns a no-op result with an explicit empty route for equal languages", () => {
    expect(translateOffline("bonjour", "fr", "fr")).toMatchObject({
      packId: null,
      packIds: [],
      pivotLanguage: null,
      text: "bonjour",
    });
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
    expect(() => parseOfflineTranslationPack("x".repeat(MAX_OFFLINE_PACK_BYTES + 1))).toThrow("1 MiB");
  });
});
