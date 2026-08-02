import type { SearchResult } from "./types";

export interface UtoolsTextCommandMatch {
  pluginId: string;
  commandId: string;
  label: string;
  matcherType: "regex" | "over" | string;
  payload: string;
}

export function utoolsTextMatcherSearchResults(
  matches: readonly UtoolsTextCommandMatch[],
): SearchResult[] {
  return matches.slice(0, 12).map((match, index) => ({
    id: `utools-matcher:${match.pluginId}:${match.commandId}:${index}`,
    name: match.label,
    kind: "plugin",
    score: 940 - index,
    metadata: `${match.matcherType === "regex" ? "正则匹配" : "任意文本"} · uTools 插件`,
    pluginId: match.pluginId,
    commandId: match.commandId,
    utoolsMatcherType: match.matcherType,
    utoolsMatcherPayload: match.payload,
  }));
}
