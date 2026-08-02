import { describe, expect, it } from "vitest";

import { utoolsContextMatcherSearchResults } from "./utools-context-matchers";

describe("uTools context matcher launcher projection", () => {
  it("keeps the native matcher index without putting paths or pixels in the result", () => {
    const [result] = utoolsContextMatcherSearchResults([{
      pluginId: "utools-image-tools",
      commandId: "utools-feature-2",
      label: "压缩图片",
      matcherType: "img",
      matcherIndex: 3,
      mainPush: false,
    }]);
    expect(result).toEqual(expect.objectContaining({
      id: "utools-context:utools-image-tools:utools-feature-2:3:0",
      utoolsMatcherType: "img",
      utoolsMatcherIndex: 3,
    }));
    expect(result).not.toHaveProperty("utoolsMatcherPayload");
    expect(result).not.toHaveProperty("path");
  });

  it("caps native context matcher results", () => {
    const matches = Array.from({ length: 20 }, (_, index) => ({
      pluginId: "utools-files",
      commandId: `utools-feature-${index}`,
      label: "处理文件",
      matcherType: "files",
      matcherIndex: index,
      mainPush: false,
    }));
    expect(utoolsContextMatcherSearchResults(matches)).toHaveLength(12);
  });

  it("projects a window matcher without renderer-owned window metadata", () => {
    const [result] = utoolsContextMatcherSearchResults([{
      pluginId: "utools-window-tools",
      commandId: "utools-feature-4",
      label: "固定记事本",
      matcherType: "window",
      matcherIndex: 1,
      mainPush: true,
    }]);

    expect(result).toEqual(expect.objectContaining({
      metadata: "窗口匹配 · uTools 插件",
      utoolsMatcherType: "window",
      utoolsMatcherIndex: 1,
      utoolsMainPush: true,
    }));
    expect(result).not.toHaveProperty("utoolsMatcherPayload");
    expect(result).not.toHaveProperty("path");
    expect(result).not.toHaveProperty("pid");
    expect(result).not.toHaveProperty("windowHandle");
  });
});
