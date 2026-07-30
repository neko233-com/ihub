import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { mergeNativeIconCache } from "../lib/native-icons";
import type { IndexStatus } from "../lib/types";
import {
  LocalSearchWorkspace,
  canOpenLocalSearchResult,
  claimLocalSearchIconBatch,
  composeLocalSearchQuery,
  filterBrowserLocalSearchResults,
  normalizeLocalSearchResults,
  settleLocalSearchIconBatch,
  shouldOpenLocalSearchResultFromKeyboard,
  sortLocalSearchResults,
} from "./LocalSearchWorkspace";

const onePixelPng = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZrG8AAAAASUVORK5CYII=";

const readyIndex: IndexStatus = {
  phase: "ready",
  indexedFiles: 1_060_482,
  roots: ["C:\\Users\\demo"],
  lastIndexedAt: "2026-07-30T19:08:21+08:00",
};

describe("LocalSearchWorkspace", () => {
  it("renders the uTools-style search, filter, result, preview, and footer regions", () => {
    const markup = renderToStaticMarkup(
      <LocalSearchWorkspace
        indexStatus={readyIndex}
        isRefreshingIndex={false}
        onClose={() => undefined}
        onOpenResult={() => undefined}
        onRefreshIndex={() => undefined}
        onSetIndexRoots={() => undefined}
        onToast={() => undefined}
      />,
    );

    expect(markup).toContain('class="local-search__header"');
    expect(markup).toContain("本地搜索");
    expect(markup).toContain('placeholder="全盘搜索"');
    expect(markup).toContain('aria-label="文件类型"');
    expect(markup).toContain("EXCEL");
    expect(markup).toContain("压缩文件");
    expect(markup).toContain("3D 模型");
    expect(markup).toContain("电子书");
    expect(markup).toContain("安装包");
    expect(markup).toContain('aria-label="本地搜索结果"');
    expect(markup).toContain('aria-label="文件预览"');
    expect(markup).toContain("所在路径");
    expect(markup).toContain("按修改时间降序");
    expect(markup).toContain("1,060,482");
    expect(markup).toContain("data:image/png;base64,");
    expect(markup).not.toContain('role="list"');
  });

  it("lets the selected category override conflicting user type filters", () => {
    expect(
      composeLocalSearchQuery(
        "quarterly ext:txt kind:file path:D:\\Work",
        "pdf",
      ),
    ).toBe("quarterly path:D:\\Work ext:pdf");
    expect(composeLocalSearchQuery("kind:folder report", "all")).toBe(
      "kind:folder report",
    );
  });

  it("keeps browser filtering inside the selected file category", () => {
    expect(
      filterBrowserLocalSearchResults("", "pdf").map((result) => result.name),
    ).toEqual(["iHub-product-spec.pdf"]);
    expect(
      filterBrowserLocalSearchResults("project", "excel").map(
        (result) => result.name,
      ),
    ).toEqual(["project-plan.xlsx"]);
    expect(
      filterBrowserLocalSearchResults("", "folder").every(
        (result) => result.kind === "folder",
      ),
    ).toBe(true);
    expect(
      filterBrowserLocalSearchResults("", "audio").map((result) => result.name),
    ).toEqual(["launch-theme.flac", "launcher-chime.opus"]);
    expect(
      filterBrowserLocalSearchResults("", "model3d").map((result) => result.name),
    ).toEqual(["mascot.glb", "assistant-avatar.vrm"]);
    expect(
      filterBrowserLocalSearchResults("", "code").map((result) => result.name),
    ).toEqual(["App.tsx", "package.json"]);
    expect(
      filterBrowserLocalSearchResults("", "ebook").map((result) => result.name),
    ).toEqual(["iHub-handbook.epub"]);
    expect(composeLocalSearchQuery("", "audio")).toContain("ape,opus,aiff");
    expect(composeLocalSearchQuery("", "model3d")).toContain("3ds,max,ma,mb,c4d,x3d,vrm");
  });

  it("announces the current keyboard selection and connects it to the query", () => {
    const markup = renderToStaticMarkup(
      <LocalSearchWorkspace
        indexStatus={readyIndex}
        isRefreshingIndex={false}
        onClose={() => undefined}
        onOpenResult={() => undefined}
        onRefreshIndex={() => undefined}
        onSetIndexRoots={() => undefined}
        onToast={() => undefined}
      />,
    );

    expect(markup).toContain('aria-controls="local-search-results"');
    expect(markup).toContain('aria-describedby="local-search-selection-status"');
    expect(markup).toContain('id="local-search-selection-status"');
    expect(markup).toContain('aria-live="polite"');
    expect(markup).toContain("当前选中：");
  });

  it("mirrors structured desktop filters in the bounded browser fixture", () => {
    expect(
      filterBrowserLocalSearchResults("ext:pdf size:>1mb", "all").map(
        (result) => result.name,
      ),
    ).toEqual(["iHub-product-spec.pdf"]);
    expect(
      filterBrowserLocalSearchResults("kind:folder path:plugins", "all").map(
        (result) => result.name,
      ),
    ).toEqual(["plugins"]);
    expect(
      filterBrowserLocalSearchResults('content:"交互设计"', "all").map(
        (result) => result.name,
      ),
    ).toEqual(["DESIGN.md"]);
    expect(
      filterBrowserLocalSearchResults(
        "modified:today",
        "all",
        new Date("2026-07-30T23:00:00Z"),
      ).map((result) => result.name),
    ).toEqual(["iHub", "App.tsx", "package.json"]);
    expect(
      filterBrowserLocalSearchResults("project ext:pdf", "excel").map(
        (result) => result.name,
      ),
    ).toEqual(["project-plan.xlsx"]);
  });

  it("never opens a result from an old or in-flight query snapshot", () => {
    expect(canOpenLocalSearchResult(null, "all\nreport", false)).toBe(false);
    expect(
      canOpenLocalSearchResult("all\nold", "all\nnew", false),
    ).toBe(false);
    expect(
      canOpenLocalSearchResult("all\nreport", "all\nreport", true),
    ).toBe(false);
    expect(
      canOpenLocalSearchResult("all\nreport", "all\nreport", false),
    ).toBe(true);
  });

  it("uses one non-repeating Enter activation while Space remains selection-only", () => {
    expect(shouldOpenLocalSearchResultFromKeyboard("Enter", false)).toBe(true);
    expect(shouldOpenLocalSearchResultFromKeyboard("Enter", true)).toBe(false);
    expect(shouldOpenLocalSearchResultFromKeyboard(" ", false)).toBe(false);
    expect(shouldOpenLocalSearchResultFromKeyboard("Space", false)).toBe(false);
  });

  it("rejects unsafe size values without discarding a valid local result", () => {
    const base = {
      id: "file-id",
      kind: "file",
      name: "example.bin",
      path: "C:\\example.bin",
      score: 1,
    };
    const normalized = normalizeLocalSearchResults([
      { ...base, id: "zero", sizeBytes: 0 },
      { ...base, id: "safe", sizeBytes: Number.MAX_SAFE_INTEGER },
      { ...base, id: "negative", sizeBytes: -1 },
      { ...base, id: "fractional", sizeBytes: 1.5 },
      { ...base, id: "unsafe", sizeBytes: Number.MAX_SAFE_INTEGER + 1 },
      { ...base, id: "string", sizeBytes: "42" },
    ]);

    expect(normalized).toHaveLength(6);
    expect(normalized[0].sizeBytes).toBe(0);
    expect(normalized[1].sizeBytes).toBe(Number.MAX_SAFE_INTEGER);
    expect(normalized.slice(2).every((result) => result.sizeBytes === undefined))
      .toBe(true);
  });

  it("sorts only the bounded current result set using explicit modes", () => {
    const results = normalizeLocalSearchResults([
      {
        id: "older",
        kind: "file",
        name: "Beta.txt",
        path: "C:\\Beta.txt",
        score: 10,
        modifiedAt: "2026-01-01T00:00:00Z",
      },
      {
        id: "newer",
        kind: "file",
        name: "Alpha.txt",
        path: "C:\\Alpha.txt",
        score: 1,
        modifiedAt: "2026-07-30T00:00:00Z",
      },
    ]);

    expect(sortLocalSearchResults(results, "modified-desc")[0].id).toBe("newer");
    expect(sortLocalSearchResults(results, "relevance")[0].id).toBe("older");
    expect(sortLocalSearchResults(results, "name-asc")[0].id).toBe("newer");
  });
});

describe("LocalSearchWorkspace native icon generations", () => {
  const results = normalizeLocalSearchResults([
    {
      id: "alpha",
      kind: "file",
      name: "alpha.exe",
      path: "C:\\Tools\\alpha.exe",
      score: 2,
    },
    {
      id: "beta",
      kind: "file",
      name: "beta.exe",
      path: "C:\\Tools\\beta.exe",
      score: 1,
    },
  ]);

  it("negative-caches an empty native response for the current search generation", () => {
    const inFlight = new Map<string, number>();
    const negative = new Map<string, number>();
    const batch = claimLocalSearchIconBatch(
      results,
      {},
      3,
      inFlight,
      negative,
    );

    expect(batch.map((result) => result.id)).toEqual(["alpha", "beta"]);
    expect(
      settleLocalSearchIconBatch(batch, {}, 3, inFlight, negative),
    ).toBe(false);
    expect(inFlight.size).toBe(0);
    expect(
      claimLocalSearchIconBatch(results, {}, 3, inFlight, negative),
    ).toEqual([]);
  });

  it("keeps missing partial-response entries negative without retrying them", () => {
    const inFlight = new Map<string, number>();
    const negative = new Map<string, number>();
    const batch = claimLocalSearchIconBatch(
      results,
      {},
      8,
      inFlight,
      negative,
    );
    const partialIcons = { alpha: onePixelPng };

    expect(
      settleLocalSearchIconBatch(batch, partialIcons, 8, inFlight, negative),
    ).toBe(true);
    const cache = mergeNativeIconCache({}, partialIcons, results);
    expect(
      claimLocalSearchIconBatch(results, cache, 8, inFlight, negative),
    ).toEqual([]);
  });

  it("re-fetches a previously successful icon after its positive cache entry is evicted", () => {
    const inFlight = new Map<string, number>();
    const negative = new Map<string, number>();
    const [result] = results;
    const firstBatch = claimLocalSearchIconBatch(
      [result],
      {},
      11,
      inFlight,
      negative,
    );

    expect(
      settleLocalSearchIconBatch(
        firstBatch,
        { alpha: onePixelPng },
        11,
        inFlight,
        negative,
      ),
    ).toBe(true);
    const populatedCache = mergeNativeIconCache(
      {},
      { alpha: onePixelPng },
      [result],
    );
    expect(
      claimLocalSearchIconBatch(
        [result],
        populatedCache,
        11,
        inFlight,
        negative,
      ),
    ).toEqual([]);
    expect(
      claimLocalSearchIconBatch([result], {}, 11, inFlight, negative).map(
        (candidate) => candidate.id,
      ),
    ).toEqual(["alpha"]);
  });

  it("negative-caches IPC failures only for the generation that failed", () => {
    const inFlight = new Map<string, number>();
    const negative = new Map<string, number>();
    const [result] = results;
    const failedBatch = claimLocalSearchIconBatch(
      [result],
      {},
      17,
      inFlight,
      negative,
    );

    expect(
      settleLocalSearchIconBatch(
        failedBatch,
        undefined,
        17,
        inFlight,
        negative,
      ),
    ).toBe(false);
    expect(
      claimLocalSearchIconBatch([result], {}, 17, inFlight, negative),
    ).toEqual([]);
    expect(
      claimLocalSearchIconBatch([result], {}, 18, inFlight, negative).map(
        (candidate) => candidate.id,
      ),
    ).toEqual(["alpha"]);
  });
});

describe("LocalSearchWorkspace visual and native-icon contracts", () => {
  const stylesheet = readFileSync(new URL("../index.css", import.meta.url), "utf8");
  const source = readFileSync(
    new URL("./LocalSearchWorkspace.tsx", import.meta.url),
    "utf8",
  );
  const appSource = readFileSync(new URL("../App.tsx", import.meta.url), "utf8");

  it("keeps all three panes visible at the native 800 logical-pixel width", () => {
    expect(stylesheet).toMatch(
      /\.toolbox-drawer\.toolbox-drawer--search \{[^}]*--local-search-surface: #2d2f31;[^}]*color-scheme: dark;/s,
    );
    expect(stylesheet).toMatch(
      /\.local-search-workspace \{[^}]*grid-template-rows: 56px minmax\(0, 1fr\) 38px;/s,
    );
    expect(stylesheet).toMatch(
      /\.local-search__main \{[^}]*grid-template-columns: 102px 300px minmax\(0, 1fr\);/s,
    );
    expect(stylesheet).toMatch(/@media \(max-width: 700px\)/);
    expect(stylesheet).not.toMatch(/@media \(max-width: 980px\)/);
    expect(appSource).toMatch(
      /const localSearchOpen = surface === "toolbox" && toolboxTab === "search";[\s\S]*?localSearchOpen\s*\?\s*602/,
    );
    expect(appSource).toContain("new LogicalSize(800, height)");
  });

  it("renders host PNG artwork transparently at its natural aspect ratio", () => {
    expect(stylesheet).toMatch(
      /\.local-search__file-icon \.result-icon \{[^}]*background: transparent;[^}]*border-radius: 0;/s,
    );
    expect(stylesheet).toMatch(
      /\.local-search__file-icon \.result-icon img \{[^}]*object-fit: contain;/s,
    );
  });

  it("bounds native icon work to one current viewport batch", () => {
    expect(source).toContain("const LOCAL_SEARCH_VISIBLE_ICON_LIMIT = 12");
    expect(source).toContain(".slice(visibleRange.start, visibleRange.end)");
    expect(source).toContain("iconQueueRef.current = iconQueueRef.current");
    expect(source).toContain("inFlightIconGenerationsRef.current.delete(identity)");
    expect(source).toContain("negativeIconGenerationsRef");
    expect(source).toMatch(
      /if \(generation !== iconGenerationRef\.current\) \{\s*return;\s*\}[\s\S]*?const response = await command<unknown>\("get_system_icons"/,
    );
    expect(source).toMatch(
      /const shouldMergeIconCache = settleLocalSearchIconBatch\([\s\S]*?if \(shouldMergeIconCache\) \{\s*setIconCache/s,
    );
  });

  it("opens native search results by current host-owned ID, not renderer path", () => {
    expect(appSource).toMatch(
      /command<void>\("open_search_result",\s*\{\s*searchResultId: result\.id,/s,
    );
  });

  it("routes focused-row Enter through the gated opener without a synthetic click", () => {
    expect(source).toMatch(
      /const openCurrentResult = \(result: SearchResult\) => \{\s*if \(!resultsAreCurrent\) \{\s*void runSearch\(\);\s*return;\s*\}[\s\S]*?const handleResultKeyDown = \([\s\S]*?event\.preventDefault\(\);\s*event\.stopPropagation\(\);\s*openCurrentResult\(result\);/s,
    );
    expect(source).toContain(
      "onKeyDown={(event) => handleResultKeyDown(event, result)}",
    );
    expect(source).toContain(
      "onClick={() => setSelectedResultId(result.id)}",
    );
    expect(source).not.toContain(
      "onClick={() => openCurrentResult(result)}",
    );
  });
});
