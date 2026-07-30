import {
  Archive,
  BookOpen,
  Box as BoxIcon,
  Check,
  ChevronDown,
  Code2,
  Database,
  File,
  FileText,
  Folder,
  FolderSearch,
  Image as ImageIcon,
  ListFilter,
  LoaderCircle,
  MoreVertical,
  Music,
  PanelRight,
  Package,
  Palette,
  Presentation,
  RefreshCw,
  Search,
  Settings2,
  Sheet,
  Trash2,
  Type,
  Video,
  X,
  type LucideIcon,
} from "lucide-react";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type PointerEvent,
  type UIEvent,
} from "react";
import { command, isDesktop } from "../lib/desktop";
import { browserPreviewSystemIcons, mockResults } from "../lib/mock-data";
import {
  mergeNativeIconCache,
  nativeIconForResult,
  sanitizeSystemIconMap,
  systemIconRequestChunks,
  type SystemIconMap,
} from "../lib/native-icons";
import type {
  IndexStatus,
  SearchResult,
  SelectedDirectoryGrant,
} from "../lib/types";
import { ResultIcon } from "./ResultIcon";

const LOCAL_SEARCH_RESULT_LIMIT = 200;
const LOCAL_SEARCH_ROW_HEIGHT = 48;
const LOCAL_SEARCH_VISIBLE_ICON_LIMIT = 12;
const LOCAL_SEARCH_ICON_CACHE_KEYS = 96;
const MAX_LOCAL_SEARCH_TEXT = 32_768;

export type LocalSearchCategoryId =
  | "all"
  | "folder"
  | "excel"
  | "word"
  | "powerpoint"
  | "pdf"
  | "image"
  | "video"
  | "audio"
  | "archive"
  | "text"
  | "code"
  | "ebook"
  | "design"
  | "model3d"
  | "font"
  | "database"
  | "installer";

export type LocalSearchSortMode = "modified-desc" | "relevance" | "name-asc";

interface LocalSearchCategory {
  id: LocalSearchCategoryId;
  label: string;
  icon: LucideIcon;
  filter: string;
}

const localSearchCategories: readonly LocalSearchCategory[] = [
  { id: "all", label: "全部", icon: Search, filter: "" },
  { id: "folder", label: "文件夹", icon: Folder, filter: "kind:folder" },
  {
    id: "excel",
    label: "EXCEL",
    icon: Sheet,
    filter: "ext:xls,xlsx,xlsm,xlsb,csv",
  },
  {
    id: "word",
    label: "WORD",
    icon: FileText,
    filter: "ext:doc,docx,docm,rtf",
  },
  {
    id: "powerpoint",
    label: "PPT",
    icon: Presentation,
    filter: "ext:ppt,pptx,pptm",
  },
  { id: "pdf", label: "PDF", icon: File, filter: "ext:pdf" },
  {
    id: "image",
    label: "图片",
    icon: ImageIcon,
    filter: "ext:png,jpg,jpeg,gif,webp,bmp,svg,ico,heic",
  },
  {
    id: "video",
    label: "视频",
    icon: Video,
    filter: "ext:mp4,mov,mkv,avi,webm,m4v,wmv",
  },
  {
    id: "audio",
    label: "音频",
    icon: Music,
    filter: "ext:mp3,wav,flac,aac,m4a,ogg,wma,ape,opus,aiff,aif,mid,midi,amr,ac3",
  },
  {
    id: "archive",
    label: "压缩文件",
    icon: Archive,
    filter: "ext:zip,7z,rar,tar,gz,bz2,xz",
  },
  {
    id: "text",
    label: "文本",
    icon: FileText,
    filter: "ext:txt,md,markdown,log,ini,cfg,conf,nfo",
  },
  {
    id: "code",
    label: "代码",
    icon: Code2,
    filter: "ext:ts,tsx,js,jsx,mjs,cjs,vue,svelte,html,htm,css,scss,sass,less,rs,go,py,java,kt,kts,swift,c,cc,cpp,h,hpp,cs,php,rb,sh,ps1,sql,json,yaml,yml,toml,xml",
  },
  {
    id: "ebook",
    label: "电子书",
    icon: BookOpen,
    filter: "ext:epub,mobi,azw,azw3,fb2,djvu,cbz,cbr",
  },
  {
    id: "design",
    label: "设计文件",
    icon: Palette,
    filter: "ext:psd,psb,ai,eps,sketch,fig,xd,afdesign,afphoto",
  },
  {
    id: "model3d",
    label: "3D 模型",
    icon: BoxIcon,
    filter: "ext:glb,gltf,fbx,obj,stl,3mf,dae,blend,ply,usd,usda,usdc,usdz,step,stp,iges,igs,3ds,max,ma,mb,c4d,x3d,vrm",
  },
  {
    id: "font",
    label: "字体",
    icon: Type,
    filter: "ext:ttf,otf,ttc,woff,woff2,eot",
  },
  {
    id: "database",
    label: "数据库",
    icon: Database,
    filter: "ext:db,sqlite,sqlite3,mdb,accdb,duckdb",
  },
  {
    id: "installer",
    label: "安装包",
    icon: Package,
    filter: "ext:exe,msi,msix,appx,appxbundle,deb,rpm,pkg,dmg,appimage,apk,ipa",
  },
] as const;

const localSearchCategoryById = new Map(
  localSearchCategories.map((category) => [category.id, category]),
);

const localSearchNameCollator = new Intl.Collator("zh-Hans-CN", {
  numeric: true,
  sensitivity: "base",
});

export function composeLocalSearchQuery(
  query: string,
  categoryId: LocalSearchCategoryId,
): string {
  const categoryFilter = localSearchCategoryById.get(categoryId)?.filter ?? "";
  const normalizedQuery = categoryFilter
    ? query
        .replace(
          /(?:^|\s)-?(?:ext|kind|type):(?:"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'|\S+)/giu,
          " ",
        )
        .trim()
        .replace(/\s+/gu, " ")
    : query.trim();
  return [normalizedQuery, categoryFilter].filter(Boolean).join(" ");
}

function safeLocalSearchText(
  value: unknown,
  maximumLength = MAX_LOCAL_SEARCH_TEXT,
): string | undefined {
  if (typeof value !== "string" || value.length === 0 || value.length > maximumLength) {
    return undefined;
  }
  return value.includes("\0") ? undefined : value;
}

/**
 * Tauri's generic is compile-time only. Keep the workspace projection narrow
 * and reject sizes that JavaScript cannot represent exactly.
 */
export function normalizeLocalSearchResults(value: unknown): SearchResult[] {
  if (!Array.isArray(value)) {
    return [];
  }
  return value.flatMap((candidate) => {
    if (!candidate || typeof candidate !== "object" || Array.isArray(candidate)) {
      return [];
    }
    const record = candidate as Record<string, unknown>;
    const id = safeLocalSearchText(record.id);
    const name = safeLocalSearchText(record.name, 2_048);
    const path = safeLocalSearchText(record.path);
    const kind = record.kind;
    if (
      !id
      || !name
      || !path
      || (kind !== "file" && kind !== "folder" && kind !== "application")
    ) {
      return [];
    }

    const score = typeof record.score === "number" && Number.isFinite(record.score)
      ? record.score
      : 0;
    const result: SearchResult = { id, kind, name, path, score };
    const metadata = safeLocalSearchText(record.metadata, 4_096);
    const modifiedAt = safeLocalSearchText(record.modifiedAt, 256);
    if (metadata) {
      result.metadata = metadata;
    }
    if (modifiedAt) {
      result.modifiedAt = modifiedAt;
    }
    if (
      typeof record.sizeBytes === "number"
      && Number.isSafeInteger(record.sizeBytes)
      && record.sizeBytes >= 0
    ) {
      result.sizeBytes = record.sizeBytes;
    }
    return [result];
  });
}

function resultExtension(result: SearchResult): string {
  const source = result.path ?? result.name;
  const fileName = source.split(/[\\/]/u).at(-1) ?? source;
  const dotIndex = fileName.lastIndexOf(".");
  return dotIndex > 0 ? fileName.slice(dotIndex + 1).toLocaleLowerCase() : "";
}

type BrowserSizePredicate = (sizeBytes: number) => boolean;

interface BrowserLocalSearchQuery {
  positiveTerms: string[];
  negativeTerms: string[];
  pathFilters: string[];
  contentTerms: string[];
  extensions: string[];
  kinds: SearchResult["kind"][];
  modifiedAfter?: number;
  sizeFilters: BrowserSizePredicate[];
}

function foldBrowserQueryText(value: string): string {
  return value.normalize("NFKC").toLocaleLowerCase();
}

function tokenizeBrowserQuery(input: string): string[] {
  const tokens: string[] = [];
  let current = "";
  let quoted = false;
  for (const character of input) {
    if (character === '"') {
      quoted = !quoted;
    } else if (/\s/u.test(character) && !quoted) {
      if (current) {
        tokens.push(current);
        current = "";
      }
    } else {
      current += character;
    }
  }
  if (current) {
    tokens.push(current);
  }
  return tokens;
}

function browserModifiedAfter(value: string, now: Date): number | undefined {
  const normalized = value.trim().toLocaleLowerCase();
  if (normalized === "today") {
    return Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate());
  }
  const days = Number(normalized.match(/^(\d+)d$/u)?.[1]);
  if (!Number.isInteger(days) || days < 1 || days > 36_500) {
    return undefined;
  }
  return now.getTime() - days * 86_400_000;
}

function browserSizeFilter(value: string): BrowserSizePredicate | undefined {
  const normalized = value.trim().toLocaleLowerCase();
  const match = normalized.match(/^(>=|<=|>|<|=)?\s*(\d+(?:\.\d+)?)\s*(b|k|kb|m|mb|g|gb|t|tb)?$/u);
  if (!match) {
    return undefined;
  }
  const multiplier = ({
    b: 1,
    k: 1_024,
    kb: 1_024,
    m: 1_024 ** 2,
    mb: 1_024 ** 2,
    g: 1_024 ** 3,
    gb: 1_024 ** 3,
    t: 1_024 ** 4,
    tb: 1_024 ** 4,
  } as Record<string, number>)[match[3] ?? "b"] ?? 1;
  const expected = Math.round(Number(match[2]) * multiplier);
  if (!Number.isSafeInteger(expected) || expected < 0) {
    return undefined;
  }
  switch (match[1] ?? "=") {
    case ">":
      return (actual) => actual > expected;
    case ">=":
      return (actual) => actual >= expected;
    case "<":
      return (actual) => actual < expected;
    case "<=":
      return (actual) => actual <= expected;
    default:
      return (actual) => actual === expected;
  }
}

function parseBrowserLocalSearchQuery(
  input: string,
  now: Date,
): BrowserLocalSearchQuery {
  const parsed: BrowserLocalSearchQuery = {
    positiveTerms: [],
    negativeTerms: [],
    pathFilters: [],
    contentTerms: [],
    extensions: [],
    kinds: [],
    sizeFilters: [],
  };
  for (const rawToken of tokenizeBrowserQuery(input)) {
    const negative = rawToken.startsWith("-");
    const token = negative ? rawToken.slice(1) : rawToken;
    if (!token) {
      continue;
    }

    if (!negative) {
      const separator = token.indexOf(":");
      if (separator > 0) {
        const field = token.slice(0, separator).toLocaleLowerCase();
        const value = token.slice(separator + 1).trim();
        if (value) {
          if (field === "path" || field === "in") {
            parsed.pathFilters.push(foldBrowserQueryText(value));
            continue;
          }
          if (field === "content" || field === "body") {
            parsed.contentTerms.push(foldBrowserQueryText(value));
            continue;
          }
          if (field === "ext") {
            const extensions = value
              .split(/[|,]/u)
              .map((extension) =>
                foldBrowserQueryText(extension.trim().replace(/^\./u, "")))
              .filter(Boolean);
            if (extensions.length) {
              parsed.extensions.push(...extensions);
              continue;
            }
          }
          if (field === "kind") {
            const kinds = value
              .split(/[|,]/u)
              .map((kind) => kind.trim().toLocaleLowerCase())
              .map((kind) => kind === "app" ? "application" : kind)
              .filter(
                (kind): kind is SearchResult["kind"] =>
                  kind === "file" || kind === "folder" || kind === "application",
              );
            if (kinds.length) {
              parsed.kinds.push(...kinds);
              continue;
            }
          }
          if (
            field === "type"
            && (value.toLocaleLowerCase() === "app"
              || value.toLocaleLowerCase() === "application")
          ) {
            parsed.kinds.push("application");
            continue;
          }
          if (field === "modified") {
            const modifiedAfter = browserModifiedAfter(value, now);
            if (modifiedAfter !== undefined) {
              parsed.modifiedAfter = Math.max(
                parsed.modifiedAfter ?? Number.NEGATIVE_INFINITY,
                modifiedAfter,
              );
              continue;
            }
          }
          if (field === "size") {
            const predicate = browserSizeFilter(value);
            if (predicate) {
              parsed.sizeFilters.push(predicate);
              continue;
            }
          }
        }
      }
    }

    const folded = foldBrowserQueryText(token);
    if (negative) {
      parsed.negativeTerms.push(folded);
    } else {
      parsed.positiveTerms.push(folded);
    }
  }
  return parsed;
}

export function filterBrowserLocalSearchResults(
  query: string,
  categoryId: LocalSearchCategoryId,
  now = new Date(),
): SearchResult[] {
  const parsed = parseBrowserLocalSearchQuery(
    composeLocalSearchQuery(query, categoryId),
    now,
  );

  return normalizeLocalSearchResults(mockResults).filter((result) => {
    if (
      parsed.extensions.length
      && !parsed.extensions.includes(resultExtension(result))
    ) {
      return false;
    }
    if (parsed.kinds.length && !parsed.kinds.includes(result.kind)) {
      return false;
    }
    const nameAndPath = [result.name, result.path]
      .filter(Boolean)
      .join(" ")
      .normalize("NFKC")
      .toLocaleLowerCase();
    const path = foldBrowserQueryText(result.path ?? "");
    const content = foldBrowserQueryText(result.metadata ?? "");
    if (!parsed.positiveTerms.every((term) => nameAndPath.includes(term))) {
      return false;
    }
    if (parsed.negativeTerms.some((term) => nameAndPath.includes(term))) {
      return false;
    }
    if (!parsed.pathFilters.every((filter) => path.includes(filter))) {
      return false;
    }
    // Browser QA has no native content index. The bounded fixture metadata is
    // its explicit body-preview projection, so `content:` still exercises the
    // same structured-query branch without implying a real disk scan.
    if (!parsed.contentTerms.every((term) => content.includes(term))) {
      return false;
    }
    if (parsed.modifiedAfter !== undefined) {
      const modifiedAt = result.modifiedAt ? Date.parse(result.modifiedAt) : Number.NaN;
      if (!Number.isFinite(modifiedAt) || modifiedAt < parsed.modifiedAfter) {
        return false;
      }
    }
    if (
      parsed.sizeFilters.length
      && (
        result.sizeBytes === undefined
        || !parsed.sizeFilters.every((predicate) => predicate(result.sizeBytes as number))
      )
    ) {
      return false;
    }
    return true;
  });
}

export function sortLocalSearchResults(
  results: readonly SearchResult[],
  mode: LocalSearchSortMode,
): SearchResult[] {
  return [...results].sort((left, right) => {
    if (mode === "relevance") {
      return right.score - left.score
        || localSearchNameCollator.compare(left.name, right.name);
    }
    if (mode === "name-asc") {
      return localSearchNameCollator.compare(left.name, right.name);
    }
    const leftTime = left.modifiedAt ? Date.parse(left.modifiedAt) : Number.NEGATIVE_INFINITY;
    const rightTime = right.modifiedAt ? Date.parse(right.modifiedAt) : Number.NEGATIVE_INFINITY;
    return rightTime - leftTime
      || localSearchNameCollator.compare(left.name, right.name);
  });
}

function formatLocalSearchBytes(result: SearchResult): string {
  if (result.kind === "folder" || result.sizeBytes === undefined) {
    return "—";
  }
  const bytes = result.sizeBytes;
  if (bytes < 1_024) {
    return `${bytes} B`;
  }
  const units = ["KB", "MB", "GB", "TB", "PB"];
  let value = bytes / 1_024;
  let unitIndex = 0;
  while (value >= 1_024 && unitIndex < units.length - 1) {
    value /= 1_024;
    unitIndex += 1;
  }
  const precision = value >= 100 ? 0 : value >= 10 ? 1 : 2;
  return `${value.toFixed(precision)} ${units[unitIndex]}`;
}

function formatLocalSearchDate(value?: string): string {
  if (!value) {
    return "—";
  }
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) {
    return "—";
  }
  return new Intl.DateTimeFormat("zh-CN", {
    dateStyle: "medium",
    timeStyle: "medium",
    hour12: false,
  }).format(date);
}

function localSearchKindLabel(kind: SearchResult["kind"]): string {
  if (kind === "folder") {
    return "文件夹";
  }
  if (kind === "application") {
    return "应用程序";
  }
  return "文件";
}

function localSearchIconRequestEligible(result: SearchResult): boolean {
  if (typeof navigator === "undefined") {
    return false;
  }
  const platform = `${navigator.platform} ${navigator.userAgent}`.toLocaleLowerCase();
  if (platform.includes("linux")) {
    return false;
  }
  if (platform.includes("mac")) {
    return result.kind === "application";
  }
  return result.kind === "file"
    || result.kind === "folder"
    || result.kind === "application";
}

function localSearchIconIdentity(result: SearchResult): string {
  return `${result.kind}\n${result.id}\n${result.path ?? ""}`;
}

export function claimLocalSearchIconBatch(
  candidates: readonly SearchResult[],
  iconCache: SystemIconMap,
  generation: number,
  inFlightGenerations: Map<string, number>,
  negativeGenerations: ReadonlyMap<string, number>,
  limit = LOCAL_SEARCH_VISIBLE_ICON_LIMIT,
): SearchResult[] {
  const batch: SearchResult[] = [];
  for (const result of candidates) {
    if (batch.length >= limit || nativeIconForResult(iconCache, result)) {
      continue;
    }
    const identity = localSearchIconIdentity(result);
    if (
      negativeGenerations.get(identity) === generation
      || inFlightGenerations.get(identity) === generation
    ) {
      continue;
    }
    inFlightGenerations.set(identity, generation);
    batch.push(result);
  }
  return batch;
}

export function settleLocalSearchIconBatch(
  batch: readonly SearchResult[],
  safeIcons: SystemIconMap | undefined,
  generation: number,
  inFlightGenerations: Map<string, number>,
  negativeGenerations: Map<string, number>,
): boolean {
  let hasSuccessfulIcon = false;
  for (const result of batch) {
    const identity = localSearchIconIdentity(result);
    if (inFlightGenerations.get(identity) === generation) {
      inFlightGenerations.delete(identity);
    }
    if (safeIcons?.[result.id]) {
      hasSuccessfulIcon = true;
      if (negativeGenerations.get(identity) === generation) {
        negativeGenerations.delete(identity);
      }
    } else {
      negativeGenerations.set(identity, generation);
    }
  }
  return hasSuccessfulIcon;
}

function localSearchRequestKey(
  query: string,
  categoryId: LocalSearchCategoryId,
): string {
  return `${categoryId}\n${query}`;
}

export function canOpenLocalSearchResult(
  completedSearchKey: string | null,
  currentSearchKey: string,
  isSearching: boolean,
): boolean {
  return !isSearching && completedSearchKey === currentSearchKey;
}

export function shouldOpenLocalSearchResultFromKeyboard(
  key: string,
  repeat: boolean,
): boolean {
  return key === "Enter" && !repeat;
}

interface LocalSearchWorkspaceProps {
  indexStatus: IndexStatus;
  isRefreshingIndex: boolean;
  onClose: () => void;
  onOpenResult: (result: SearchResult) => Promise<void> | void;
  onRefreshIndex: () => void;
  onSetIndexRoots: (
    roots: string[],
    directoryOpenIds: string[],
  ) => Promise<void> | void;
  onStartWindowDrag?: () => void;
  onToast: (message: string) => void;
}

export function LocalSearchWorkspace({
  indexStatus,
  isRefreshingIndex,
  onClose,
  onOpenResult,
  onRefreshIndex,
  onSetIndexRoots,
  onStartWindowDrag,
  onToast,
}: LocalSearchWorkspaceProps) {
  const previewResults = useMemo(
    () => filterBrowserLocalSearchResults("", "all"),
    [],
  );
  const [query, setQuery] = useState("");
  const [activeCategory, setActiveCategory] = useState<LocalSearchCategoryId>("all");
  const [sortMode, setSortMode] = useState<LocalSearchSortMode>("modified-desc");
  const [rawResults, setRawResults] = useState<SearchResult[]>(() =>
    isDesktop() ? [] : previewResults);
  const [selectedResultId, setSelectedResultId] = useState<string | null>(() =>
    isDesktop() ? null : previewResults[0]?.id ?? null);
  const [completedSearchKey, setCompletedSearchKey] = useState<string | null>(() =>
    isDesktop() ? null : localSearchRequestKey("", "all"));
  const [previewEnabled, setPreviewEnabled] = useState(true);
  const [isSearching, setIsSearching] = useState(isDesktop);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [editedIndexRoots, setEditedIndexRoots] = useState<string[]>(indexStatus.roots);
  const [selectedIndexRootOpenIds, setSelectedIndexRootOpenIds] = useState(
    () => new Map<string, string>(),
  );
  const [isSavingIndexRoots, setIsSavingIndexRoots] = useState(false);
  const [isChoosingIndexRoot, setIsChoosingIndexRoot] = useState(false);
  const [visibleRange, setVisibleRange] = useState({ start: 0, end: 12 });
  const [settledIconIdentities, setSettledIconIdentities] = useState(
    () => new Set<string>(),
  );
  const [iconCache, setIconCache] = useState<SystemIconMap>(() =>
    isDesktop()
      ? {}
      : mergeNativeIconCache(
          {},
          browserPreviewSystemIcons,
          normalizeLocalSearchResults(mockResults),
          [],
          LOCAL_SEARCH_ICON_CACHE_KEYS,
        ));
  const searchRequestRef = useRef(0);
  const iconGenerationRef = useRef(0);
  const inFlightIconGenerationsRef = useRef(new Map<string, number>());
  const negativeIconGenerationsRef = useRef(new Map<string, number>());
  const iconQueueRef = useRef<Promise<void>>(Promise.resolve());
  const resultListRef = useRef<HTMLDivElement | null>(null);
  const resultRowsRef = useRef(new Map<string, HTMLButtonElement>());

  const results = useMemo(
    () => sortLocalSearchResults(rawResults, sortMode),
    [rawResults, sortMode],
  );
  const selectedResult = useMemo(
    () => results.find((result) => result.id === selectedResultId) ?? results[0],
    [results, selectedResultId],
  );
  const currentSearchKey = localSearchRequestKey(query, activeCategory);
  const resultsAreCurrent = canOpenLocalSearchResult(
    completedSearchKey,
    currentSearchKey,
    isSearching,
  );
  const searchPending = isSearching || completedSearchKey !== currentSearchKey;
  const selectedResultAnnouncement = selectedResult
    ? `当前选中：${selectedResult.name}，${selectedResult.path || localSearchKindLabel(selectedResult.kind)}`
    : results.length
      ? `共 ${results.length} 个结果`
      : "当前没有搜索结果";

  const runSearch = useCallback(async () => {
    const requestId = searchRequestRef.current + 1;
    const requestKey = localSearchRequestKey(query, activeCategory);
    searchRequestRef.current = requestId;
    setIsSearching(true);
    setSearchError(null);

    try {
      const nextResults = isDesktop()
        ? normalizeLocalSearchResults(await command<unknown>("search_entries", {
            query: composeLocalSearchQuery(query, activeCategory),
            limit: LOCAL_SEARCH_RESULT_LIMIT,
          }))
        : filterBrowserLocalSearchResults(query, activeCategory);
      if (requestId !== searchRequestRef.current) {
        return;
      }
      iconGenerationRef.current += 1;
      inFlightIconGenerationsRef.current.clear();
      negativeIconGenerationsRef.current.clear();
      setSettledIconIdentities(new Set());
      setVisibleRange({ start: 0, end: LOCAL_SEARCH_VISIBLE_ICON_LIMIT });
      setRawResults(nextResults);
      setCompletedSearchKey(requestKey);
      setSelectedResultId((current) =>
        nextResults.some((result) => result.id === current)
          ? current
          : nextResults[0]?.id ?? null);
      resultListRef.current?.scrollTo({ top: 0 });
    } catch (error) {
      if (requestId !== searchRequestRef.current) {
        return;
      }
      setRawResults([]);
      setCompletedSearchKey(requestKey);
      setSelectedResultId(null);
      setSearchError(error instanceof Error ? error.message : "本地搜索暂不可用。");
    } finally {
      if (requestId === searchRequestRef.current) {
        setIsSearching(false);
      }
    }
  }, [activeCategory, query]);

  useEffect(() => {
    const timer = window.setTimeout(() => {
      void runSearch();
    }, 55);
    return () => window.clearTimeout(timer);
  }, [indexStatus.lastIndexedAt, runSearch]);

  useEffect(() => {
    if (!results.length) {
      setSelectedResultId(null);
      return;
    }
    if (!results.some((result) => result.id === selectedResultId)) {
      setSelectedResultId(results[0].id);
    }
  }, [results, selectedResultId]);

  useEffect(() => {
    if (!selectedResultId) {
      return;
    }
    resultRowsRef.current.get(selectedResultId)?.scrollIntoView({
      block: "nearest",
    });
  }, [selectedResultId]);

  useEffect(() => {
    if (!isDesktop()) {
      return;
    }
    const generation = iconGenerationRef.current;
    const batch = claimLocalSearchIconBatch(
      results
      .slice(visibleRange.start, visibleRange.end)
        .filter(localSearchIconRequestEligible),
      iconCache,
      generation,
      inFlightIconGenerationsRef.current,
      negativeIconGenerationsRef.current,
      LOCAL_SEARCH_VISIBLE_ICON_LIMIT,
    );
    const request = systemIconRequestChunks(
      batch.map((result) => result.id),
      [],
      LOCAL_SEARCH_VISIBLE_ICON_LIMIT,
    )[0];
    if (!request) {
      return;
    }
    setSettledIconIdentities((current) => {
      const next = new Set(current);
      batch.forEach((result) => next.delete(localSearchIconIdentity(result)));
      return next;
    });

    iconQueueRef.current = iconQueueRef.current
      .catch(() => undefined)
      .then(async () => {
        if (generation !== iconGenerationRef.current) {
          return;
        }
        let safeIcons: SystemIconMap | undefined;
        try {
          const response = await command<unknown>("get_system_icons", {
            searchResultIds: request.searchResultIds,
            launcherShortcutIds: request.launcherShortcutIds,
          });
          if (generation !== iconGenerationRef.current) {
            return;
          }
          safeIcons = sanitizeSystemIconMap(
            response,
            new Set(request.searchResultIds),
          );
        } catch {
          // A neutral file glyph remains available when the native worker is
          // unsupported, busy, or times out. Do not automatically retry-loop.
        } finally {
          if (generation === iconGenerationRef.current) {
            const shouldMergeIconCache = settleLocalSearchIconBatch(
              batch,
              safeIcons,
              generation,
              inFlightIconGenerationsRef.current,
              negativeIconGenerationsRef.current,
            );
            if (shouldMergeIconCache) {
              setIconCache((current) =>
                mergeNativeIconCache(
                  current,
                  safeIcons ?? {},
                  batch,
                  [],
                  LOCAL_SEARCH_ICON_CACHE_KEYS,
                ));
            }
            setSettledIconIdentities((current) => {
              const next = new Set(current);
              batch.forEach((result) =>
                next.add(localSearchIconIdentity(result)));
              return next;
            });
          } else {
            batch.forEach((result) => {
              const identity = localSearchIconIdentity(result);
              if (inFlightIconGenerationsRef.current.get(identity) === generation) {
                inFlightIconGenerationsRef.current.delete(identity);
              }
            });
          }
        }
      });
  }, [iconCache, results, visibleRange]);

  const moveSelection = (offset: -1 | 1) => {
    if (!results.length) {
      return;
    }
    const currentIndex = Math.max(
      0,
      results.findIndex((result) => result.id === selectedResult?.id),
    );
    const nextIndex = Math.min(
      results.length - 1,
      Math.max(0, currentIndex + offset),
    );
    setSelectedResultId(results[nextIndex].id);
  };

  const handleQueryKeyDown = (event: KeyboardEvent<HTMLInputElement>) => {
    if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      moveSelection(event.key === "ArrowDown" ? 1 : -1);
      return;
    }
    if (event.key === "Enter") {
      event.preventDefault();
      if (resultsAreCurrent && selectedResult) {
        void onOpenResult(selectedResult);
      } else {
        void runSearch();
      }
    }
  };

  const openCurrentResult = (result: SearchResult) => {
    if (!resultsAreCurrent) {
      void runSearch();
      return;
    }
    void onOpenResult(result);
  };

  const handleResultKeyDown = (
    event: KeyboardEvent<HTMLButtonElement>,
    result: SearchResult,
  ) => {
    if (!shouldOpenLocalSearchResultFromKeyboard(event.key, event.repeat)) {
      return;
    }
    // Cancel the button's synthesized click so one Enter press cannot both
    // activate this path and fall through to the row's selection click.
    event.preventDefault();
    event.stopPropagation();
    openCurrentResult(result);
  };

  const handleWorkspaceKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.key !== "Escape") {
      return;
    }
    event.preventDefault();
    event.stopPropagation();
    if (settingsOpen) {
      setSettingsOpen(false);
    } else if (query) {
      setQuery("");
    } else {
      onClose();
    }
  };

  const handleHeaderPointerDown = (event: PointerEvent<HTMLElement>) => {
    if (
      event.button !== 0
      || (event.target as HTMLElement).closest("button, input, select, label")
    ) {
      return;
    }
    onStartWindowDrag?.();
  };

  const handleResultScroll = (event: UIEvent<HTMLDivElement>) => {
    const viewport = event.currentTarget;
    const firstVisible = Math.max(
      0,
      Math.floor(viewport.scrollTop / LOCAL_SEARCH_ROW_HEIGHT) - 1,
    );
    const nextRange = {
      start: firstVisible,
      end: Math.min(
        results.length,
        firstVisible + LOCAL_SEARCH_VISIBLE_ICON_LIMIT,
      ),
    };
    setVisibleRange((current) =>
      current.start === nextRange.start && current.end === nextRange.end
        ? current
        : nextRange);
  };

  const toggleSettings = () => {
    setSettingsOpen((current) => {
      if (!current) {
        setEditedIndexRoots(indexStatus.roots);
        setSelectedIndexRootOpenIds(new Map());
      }
      return !current;
    });
  };

  const addIndexRoot = (selection: SelectedDirectoryGrant) => {
    const root = selection.path;
    if (
      editedIndexRoots.some(
        (current) => current.trim().toLocaleLowerCase() === root.toLocaleLowerCase(),
      )
    ) {
      onToast("这个目录已经在索引范围中。");
      return;
    }
    setEditedIndexRoots((current) => [...current, root]);
    setSelectedIndexRootOpenIds((current) => {
      const next = new Map(current);
      next.set(root, selection.openId);
      return next;
    });
  };

  const chooseIndexRoot = async () => {
    if (!isDesktop()) {
      onToast("浏览器预览不会打开本机文件夹选择器。");
      return;
    }
    setIsChoosingIndexRoot(true);
    try {
      const selection = await command<SelectedDirectoryGrant | null>("select_directory");
      if (selection) {
        addIndexRoot(selection);
      }
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法打开系统文件夹选择器。");
    } finally {
      setIsChoosingIndexRoot(false);
    }
  };

  const saveIndexRoots = async (roots = editedIndexRoots) => {
    if (!isDesktop()) {
      onToast("浏览器预览不会修改本地索引目录。");
      return;
    }
    setIsSavingIndexRoots(true);
    try {
      const directoryOpenIds = roots.flatMap((root) => {
        const openId = selectedIndexRootOpenIds.get(root);
        return openId ? [openId] : [];
      });
      await onSetIndexRoots(roots, directoryOpenIds);
      setEditedIndexRoots(roots);
      setSelectedIndexRootOpenIds(new Map());
      onToast(
        roots.length
          ? "已保存索引目录并开始重新扫描。"
          : "已恢复默认索引目录并开始重新扫描。",
      );
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法保存索引目录。");
    } finally {
      setIsSavingIndexRoots(false);
    }
  };

  const indexPhaseLabel = indexStatus.phase === "ready"
    ? "索引就绪"
    : indexStatus.phase === "scanning"
      ? "正在建立索引"
      : indexStatus.phase === "error"
        ? "索引异常"
        : "等待索引";

  return (
    <section
      aria-label="本地搜索工作台"
      className={`local-search-workspace${previewEnabled ? "" : " is-preview-hidden"}`}
      id="toolbox-panel-search"
      onKeyDownCapture={handleWorkspaceKeyDown}
      role="tabpanel"
    >
      <header
        className="local-search__header"
        onPointerDown={handleHeaderPointerDown}
      >
        <div className="local-search__scope-group">
          <div className="local-search__scope">
            <Search aria-hidden="true" size={23} strokeWidth={2.2} />
            <h2 id="local-search-title">本地搜索</h2>
          </div>
          <div className="local-search__mode">
            <span>搜索</span>
            <button aria-label="关闭本地搜索" onClick={onClose} type="button">
              <X size={17} />
            </button>
          </div>
        </div>
        <label className="local-search__query">
          <span className="sr-only">全盘搜索</span>
          <input
            aria-controls="local-search-results"
            aria-describedby="local-search-selection-status"
            autoFocus
            maxLength={MAX_LOCAL_SEARCH_TEXT}
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={handleQueryKeyDown}
            placeholder="全盘搜索"
            spellCheck={false}
            value={query}
          />
          <span
            aria-atomic="true"
            aria-live="polite"
            className="sr-only"
            id="local-search-selection-status"
            role="status"
          >
            {selectedResultAnnouncement}
          </span>
        </label>
        <button
          aria-expanded={settingsOpen}
          aria-label="本地搜索设置"
          className="local-search__more"
          onClick={toggleSettings}
          type="button"
        >
          <MoreVertical size={27} />
        </button>
        <button
          aria-label="立即搜索"
          className="local-search__submit"
          disabled={isSearching}
          onClick={() => void runSearch()}
          type="button"
        >
          {searchPending
            ? <LoaderCircle className="spin" size={31} />
            : <Search size={34} strokeWidth={2.1} />}
          <span>↵</span>
        </button>
      </header>

      <div className="local-search__main">
        <nav aria-label="文件类型" className="local-search__categories">
          {localSearchCategories.map((category) => {
            const Icon = category.icon;
            const selected = activeCategory === category.id;
            return (
              <button
                aria-current={selected ? "page" : undefined}
                className={selected ? "is-active" : ""}
                key={category.id}
                onClick={() => setActiveCategory(category.id)}
                type="button"
              >
                <Icon aria-hidden="true" size={16} />
                <span>{category.label}</span>
              </button>
            );
          })}
        </nav>

        <div
          aria-label="本地搜索结果"
          aria-busy={searchPending}
          className="local-search__results"
          id="local-search-results"
          onScroll={handleResultScroll}
          ref={resultListRef}
        >
          {searchError ? (
            <div className="local-search__empty" role="alert">
              <FolderSearch size={34} />
              <strong>本地搜索暂不可用</strong>
              <span>{searchError}</span>
              <button onClick={() => void runSearch()} type="button">重试</button>
            </div>
          ) : results.length ? (
            results.map((result) => {
              const selected = selectedResult?.id === result.id;
              const nativeIconSrc = nativeIconForResult(iconCache, result);
              const nativeIconPending = isDesktop()
                && localSearchIconRequestEligible(result)
                && !nativeIconSrc
                && !settledIconIdentities.has(localSearchIconIdentity(result));
              return (
                <button
                  aria-current={selected ? "true" : undefined}
                  className={`local-search__result${selected ? " is-selected" : ""}`}
                  key={`${result.kind}:${result.id}`}
                  onClick={() => setSelectedResultId(result.id)}
                  onDoubleClick={() => openCurrentResult(result)}
                  onKeyDown={(event) => handleResultKeyDown(event, result)}
                  ref={(element) => {
                    if (element) {
                      resultRowsRef.current.set(result.id, element);
                    } else {
                      resultRowsRef.current.delete(result.id);
                    }
                  }}
                  type="button"
                >
                  <span className="local-search__file-icon">
                    <ResultIcon
                      iconSrc={nativeIconSrc}
                      kind={result.kind}
                      nativeIconPending={nativeIconPending}
                    />
                  </span>
                  <span className="local-search__result-copy">
                    <strong>{result.name}</strong>
                    <small title={result.path}>{result.path}</small>
                  </span>
                </button>
              );
            })
          ) : (
            <div className="local-search__empty">
              {isSearching
                ? <LoaderCircle className="spin" size={34} />
                : <FolderSearch size={34} />}
              <strong>{isSearching ? "正在搜索…" : "没有找到匹配项目"}</strong>
              <span>试试文件名、路径、拼音，或切换左侧文件类型。</span>
            </div>
          )}
        </div>

        {previewEnabled ? (
          <aside aria-label="文件预览" className="local-search__preview">
            {selectedResult ? (
              <>
                <div className="local-search__preview-identity">
                  <span className="local-search__preview-icon">
                    <ResultIcon
                      iconSrc={nativeIconForResult(iconCache, selectedResult)}
                      kind={selectedResult.kind}
                      nativeIconPending={
                        isDesktop()
                        && localSearchIconRequestEligible(selectedResult)
                        && !nativeIconForResult(iconCache, selectedResult)
                        && !settledIconIdentities.has(
                          localSearchIconIdentity(selectedResult),
                        )
                      }
                    />
                  </span>
                  <h3 title={selectedResult.name}>{selectedResult.name}</h3>
                  <span>{localSearchKindLabel(selectedResult.kind)}</span>
                </div>
                <dl className="local-search__metadata">
                  <div>
                    <dt>大小</dt>
                    <dd>{formatLocalSearchBytes(selectedResult)}</dd>
                  </div>
                  <div>
                    <dt>修改时间</dt>
                    <dd>{formatLocalSearchDate(selectedResult.modifiedAt)}</dd>
                  </div>
                  <div>
                    <dt>所在路径</dt>
                    <dd title={selectedResult.path}>{selectedResult.path}</dd>
                  </div>
                </dl>
                <button
                  className="local-search__open"
                  disabled={!resultsAreCurrent}
                  onClick={() => openCurrentResult(selectedResult)}
                  type="button"
                >
                  打开项目
                </button>
              </>
            ) : (
              <div className="local-search__preview-empty">
                <PanelRight size={38} />
                <strong>选择一个项目查看详情</strong>
              </div>
            )}
          </aside>
        ) : null}
      </div>

      <footer className="local-search__footer">
        <button
          aria-expanded={settingsOpen}
          aria-label="打开索引设置"
          className="local-search__settings-button"
          onClick={toggleSettings}
          type="button"
        >
          <Settings2 size={21} />
        </button>
        <label className="local-search__sort">
          <ListFilter size={18} />
          <span className="sr-only">结果排序</span>
          <select
            onChange={(event) => setSortMode(event.target.value as LocalSearchSortMode)}
            value={sortMode}
          >
            <option value="modified-desc">按修改时间降序</option>
            <option value="relevance">按匹配度排序</option>
            <option value="name-asc">按名称升序</option>
          </select>
          <ChevronDown size={14} />
        </label>
        <button
          aria-checked={previewEnabled}
          className="local-search__preview-toggle"
          onClick={() => setPreviewEnabled((current) => !current)}
          role="switch"
          type="button"
        >
          <span>开启文件预览</span>
          <span className="local-search__switch" aria-hidden="true">
            <span />
          </span>
        </button>
        <div aria-live="polite" className="local-search__count">
          {rawResults.length >= LOCAL_SEARCH_RESULT_LIMIT ? "前 " : ""}
          <strong>{new Intl.NumberFormat("zh-CN").format(results.length)}</strong>
          {" 条 · 索引 "}
          <strong>{new Intl.NumberFormat("zh-CN").format(indexStatus.indexedFiles)}</strong>
          {" 项"}
        </div>
      </footer>

      {settingsOpen ? (
        <aside aria-label="本地搜索设置" className="local-search__settings">
          <header>
            <div>
              <span>LOCAL INDEX</span>
              <h3>本地搜索设置</h3>
            </div>
            <button aria-label="关闭本地搜索设置" onClick={() => setSettingsOpen(false)} type="button">
              <X size={19} />
            </button>
          </header>
          <div className="local-search__settings-status">
            <span className={`is-${indexStatus.phase}`} />
            <div>
              <strong>{indexPhaseLabel}</strong>
              <small>
                {new Intl.NumberFormat("zh-CN").format(indexStatus.indexedFiles)}
                {" 个项目"}
              </small>
            </div>
            <button
              disabled={isRefreshingIndex}
              onClick={onRefreshIndex}
              type="button"
            >
              {isRefreshingIndex
                ? <LoaderCircle className="spin" size={15} />
                : <RefreshCw size={15} />}
              刷新
            </button>
          </div>
          <label className="local-search__root-input">
            <span>索引目录</span>
            <div>
              <input
                aria-label="索引目录只能通过系统文件夹选择器添加"
                disabled={isSavingIndexRoots}
                placeholder="请使用右侧系统选择器"
                readOnly
                value=""
              />
              <button
                disabled={isChoosingIndexRoot || isSavingIndexRoots}
                onClick={() => void chooseIndexRoot()}
                type="button"
              >
                {isChoosingIndexRoot
                  ? <LoaderCircle className="spin" size={15} />
                  : <FolderSearch size={15} />}
                选择
              </button>
            </div>
          </label>
          <ul className="local-search__root-list">
            {editedIndexRoots.length ? editedIndexRoots.map((root) => (
              <li key={root}>
                <code title={root}>{root}</code>
                <button
                  aria-label={`移除索引目录 ${root}`}
                  disabled={isSavingIndexRoots}
                  onClick={() => {
                    setEditedIndexRoots((current) =>
                      current.filter((entry) => entry !== root));
                    setSelectedIndexRootOpenIds((current) => {
                      const next = new Map(current);
                      next.delete(root);
                      return next;
                    });
                  }}
                  type="button"
                >
                  <Trash2 size={14} />
                </button>
              </li>
            )) : (
              <li className="is-empty">保存后使用系统默认目录。</li>
            )}
          </ul>
          <div className="local-search__settings-actions">
            <button
              disabled={isSavingIndexRoots}
              onClick={() => void saveIndexRoots([])}
              type="button"
            >
              恢复默认
            </button>
            <button
              className="is-primary"
              disabled={isSavingIndexRoots}
              onClick={() => void saveIndexRoots()}
              type="button"
            >
              {isSavingIndexRoots
                ? <LoaderCircle className="spin" size={15} />
                : <Check size={15} />}
              保存并重扫
            </button>
          </div>
          <p>
            支持文件名、路径、拼音，以及 <code>ext:</code>、<code>kind:</code>、
            <code>modified:</code>、<code>size:</code> 和 <code>content:</code> 过滤。
          </p>
        </aside>
      ) : null}
    </section>
  );
}
