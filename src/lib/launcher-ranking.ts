import type { SearchKind, SearchResult } from "./types";

/**
 * The native index already produces an efficient, relevance-sorted window of
 * filesystem results. The launcher also has commands, plugin providers, and
 * local content, so it needs one renderer-side ordering pass instead of
 * accidentally placing every filesystem match after those fixed groups.
 */
const kindWeight: Record<SearchKind, number> = {
  application: 260,
  folder: 240,
  file: 230,
  plugin: 180,
  command: 170,
};

function normalized(value?: string) {
  return (value ?? "").trim().replace(/\s+/g, " ").toLocaleLowerCase();
}

function boundedScore(value: number) {
  return Number.isFinite(value) ? Math.max(-1_000, Math.min(1_000, value)) : 0;
}

function textMatchWeight(result: SearchResult, query: string) {
  if (!query) {
    return 0;
  }

  const name = normalized(result.name);
  if (name === query) {
    return 4_000;
  }
  if (name.startsWith(query)) {
    return 3_000;
  }
  if (name.includes(query)) {
    return 2_000;
  }

  const path = normalized(result.path);
  if (path.startsWith(query)) {
    return 1_200;
  }
  if (path.includes(query)) {
    return 900;
  }

  return normalized(result.metadata).includes(query) ? 650 : 0;
}

/** Exposed for focused unit coverage of the cross-source ordering contract. */
export function launcherResultRank(result: SearchResult, query: string) {
  // A deliberate mathematical expression is an explicit Spotlight intent.
  // Keep its host-calculated answer ahead of a coincidentally named file such
  // as `2+2.txt`; ordinary bare numbers never receive this marker.
  if (
    result.calculatorExpression
    && normalized(result.calculatorExpression) === normalized(query)
  ) {
    return 10_000 + boundedScore(result.score);
  }
  if (result.timeInput && normalized(result.timeInput) === normalized(query)) {
    return 9_500 + boundedScore(result.score);
  }
  return textMatchWeight(result, normalized(query)) + kindWeight[result.kind] + boundedScore(result.score);
}

function identityFor(result: SearchResult) {
  return result.pluginId && result.commandId ? `${result.pluginId}:${result.commandId}` : result.id;
}

interface RankedResult {
  result: SearchResult;
  rank: number;
  order: number;
}

/**
 * Deduplicates and merges host, builtin, plugin, and local-content results.
 * A duplicate takes its best relevance score rather than whichever source was
 * appended first. Stable source order remains the final tie-breaker.
 */
export function mergeLauncherSearchResults(
  query: string,
  ...groups: ReadonlyArray<readonly SearchResult[]>
): SearchResult[] {
  const ranked: RankedResult[] = [];
  let order = 0;
  for (const group of groups) {
    for (const result of group) {
      ranked.push({ result, rank: launcherResultRank(result, query), order });
      order += 1;
    }
  }

  ranked.sort((left, right) =>
    right.rank - left.rank
    || right.result.score - left.result.score
    || left.order - right.order,
  );

  const seen = new Set<string>();
  return ranked.flatMap(({ result }) => {
    const identity = identityFor(result);
    if (seen.has(identity)) {
      return [];
    }
    seen.add(identity);
    return [result];
  });
}
