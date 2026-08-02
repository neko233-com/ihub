import { describe, expect, it } from "vitest";

import { utoolsTextMatcherSearchResults } from "./utools-text-matchers";

describe("uTools text matcher launcher projection", () => {
  it("preserves the host-issued matcher type and payload on a bounded result", () => {
    expect(utoolsTextMatcherSearchResults([{
      pluginId: "utools-demo",
      commandId: "utools-feature-1",
      label: "打开网址",
      matcherType: "regex",
      matcherIndex: 2,
      mainPush: true,
      payload: "https://example.com",
    }])).toEqual([expect.objectContaining({
      id: "utools-matcher:utools-demo:utools-feature-1:0",
      name: "打开网址",
      utoolsMatcherType: "regex",
      utoolsMatcherIndex: 2,
      utoolsMatcherPayload: "https://example.com",
      utoolsMainPush: true,
    })]);
  });

  it("never projects more than the native result cap", () => {
    const matches = Array.from({ length: 20 }, (_, index) => ({
      pluginId: "utools-demo",
      commandId: `command-${index}`,
      label: "匹配",
      matcherType: "over",
      matcherIndex: index,
      mainPush: false,
      payload: "text",
    }));
    expect(utoolsTextMatcherSearchResults(matches)).toHaveLength(12);
  });
});
