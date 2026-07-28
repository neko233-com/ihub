import { describe, expect, it } from "vitest";
import { launcherResultRank, mergeLauncherSearchResults } from "./launcher-ranking";
import type { SearchResult } from "./types";

function result(overrides: Partial<SearchResult> = {}): SearchResult {
  return {
    id: "result",
    name: "Untitled",
    kind: "command",
    score: 0,
    ...overrides,
  };
}

describe("launcher result ranking", () => {
  it("keeps an exact local file ahead of a merely matching command", () => {
    const ordered = mergeLauncherSearchResults(
      "budget",
      [result({ id: "command", name: "Budget planner", kind: "command", score: 980 })],
      [result({ id: "file", name: "budget", kind: "file", score: 520, path: "D:/notes/budget" })],
    );

    expect(ordered.map((item) => item.id)).toEqual(["file", "command"]);
  });

  it("keeps a direct command ahead of a file whose match only appears inside its name", () => {
    const ordered = mergeLauncherSearchResults(
      "json",
      [result({ id: "command", name: "JSON 格式化与校验", kind: "command", score: 980 })],
      [result({ id: "file", name: "project-json-notes.md", kind: "file", score: 540 })],
    );

    expect(ordered.map((item) => item.id)).toEqual(["command", "file"]);
  });

  it("keeps an intentional calculator expression ahead of a coincidentally named file", () => {
    const ordered = mergeLauncherSearchResults(
      "2 + 2",
      [result({
        id: "calculator",
        name: "2 + 2 = 4",
        kind: "command",
        score: 1_000,
        calculatorExpression: "2 + 2",
      })],
      [result({ id: "file", name: "2 + 2", kind: "file", score: 1_000 })],
    );

    expect(ordered.map((item) => item.id)).toEqual(["calculator", "file"]);
  });

  it("keeps a recognized timestamp conversion ahead of a coincidentally named file", () => {
    const ordered = mergeLauncherSearchResults(
      "1700000000000",
      [result({
        id: "time",
        name: "时间与时间戳",
        kind: "command",
        score: 980,
        timeInput: "1700000000000",
      })],
      [result({ id: "file", name: "1700000000000", kind: "file", score: 1_000 })],
    );

    expect(ordered.map((item) => item.id)).toEqual(["time", "file"]);
  });

  it("chooses the highest-ranked duplicate instead of the first appended source", () => {
    const duplicateCommand = result({
      id: "duplicate",
      name: "Needle helper",
      kind: "command",
      score: 980,
      pluginId: "example",
      commandId: "open",
    });
    const duplicateFile = result({
      id: "duplicate-file",
      name: "needle",
      kind: "file",
      score: 520,
      pluginId: "example",
      commandId: "open",
    });

    const ordered = mergeLauncherSearchResults("needle", [duplicateCommand], [duplicateFile]);

    expect(ordered).toHaveLength(1);
    expect(ordered[0]?.id).toBe("duplicate-file");
    expect(launcherResultRank(duplicateFile, "needle")).toBeGreaterThan(
      launcherResultRank(duplicateCommand, "needle"),
    );
  });
});
