import { check, type Update } from "@tauri-apps/plugin-updater";
import {
  Activity,
  ArrowRight,
  Check,
  CircleAlert,
  ChevronRight,
  Command as CommandIcon,
  Download,
  GitBranch,
  HardDrive,
  LoaderCircle,
  Plus,
  Power,
  RefreshCw,
  Search,
  Settings2,
  Sparkles,
  X,
  Zap,
} from "lucide-react";
import { AnimatePresence, motion, useReducedMotion } from "motion/react";
import {
  type KeyboardEvent as ReactKeyboardEvent,
  type MouseEvent,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { BlurText } from "./components/BlurText";
import { PluginFrontendFrame } from "./components/PluginFrontendFrame";
import { PluginDrawer } from "./components/PluginDrawer";
import { ResultIcon } from "./components/ResultIcon";
import { command, isDesktop, onFocusSearch } from "./lib/desktop";
import { mockPlugins, mockResults } from "./lib/mock-data";
import type {
  AppHealth,
  AutostartStatus,
  IndexStatus,
  PluginCommandInfo,
  PluginFrontendEvent,
  PluginInfo,
  SearchResult,
} from "./lib/types";

const browserStatus: IndexStatus = {
  phase: "ready",
  indexedFiles: 0,
  roots: [],
  message: "浏览器预览模式",
};

type UpdatePhase =
  | "idle"
  | "checking"
  | "available"
  | "downloading"
  | "installing"
  | "installed"
  | "error";

interface UpdateProgress {
  received: number;
  total?: number;
}

const platformShortcut = () =>
  navigator.userAgent.toLowerCase().includes("mac") ? "⌘ K" : "Ctrl K";

function filterPreviewResults(query: string) {
  const normalized = query.trim().toLocaleLowerCase();
  if (!normalized) {
    return mockResults;
  }

  return mockResults.filter((item) =>
    [item.name, item.metadata, item.path]
      .filter(Boolean)
      .join(" ")
      .toLocaleLowerCase()
      .includes(normalized),
  );
}

function statusLabel(status: IndexStatus) {
  if (status.phase === "scanning") {
    return "正在索引";
  }
  if (status.phase === "error") {
    return "索引需要注意";
  }
  if (!isDesktop()) {
    return "预览模式";
  }
  return "索引已就绪";
}

function pluginCommandResults(plugins: PluginInfo[], query: string): SearchResult[] {
  const normalized = query.trim().toLocaleLowerCase();

  return plugins.flatMap((plugin, pluginIndex) => {
    if (plugin.enabled === false || !Array.isArray(plugin.commands)) {
      return [];
    }

    return plugin.commands
      .filter((command): command is PluginCommandInfo => Boolean(command?.id))
      .filter((command) => {
        if (!normalized) {
          return true;
        }
        return [plugin.name, plugin.description, command.id, command.name, command.description]
          .filter(Boolean)
          .join(" ")
          .toLocaleLowerCase()
          .includes(normalized);
      })
      .map((command, commandIndex) => ({
        id: `plugin-command:${plugin.id}:${command.id}`,
        name: command.name || command.id,
        kind: "plugin" as const,
        score: 900 - pluginIndex * 10 - commandIndex,
        metadata: [plugin.name, command.description].filter(Boolean).join(" · "),
        pluginId: plugin.id,
        commandId: command.id,
      }));
  });
}

function createFrontendCommandEvent(pluginId: string, commandId: string): PluginFrontendEvent {
  const suffix =
    typeof crypto !== "undefined" && "randomUUID" in crypto
      ? crypto.randomUUID()
      : Math.random().toString(36).slice(2);
  const requestId = `launcher-${Date.now().toString(36)}-${suffix}`;

  return {
    id: requestId,
    pluginId,
    name: `ihub://plugin/${pluginId}/command`,
    payload: {
      requestId,
      commandId,
      input: null,
      context: null,
    },
  };
}

export function App() {
  const inputRef = useRef<HTMLInputElement>(null);
  const booted = useRef(false);
  const updateRef = useRef<Update | null>(null);
  const approvedNativePlugins = useRef(new Set<string>());
  const prefersReducedMotion = useReducedMotion();
  const [query, setQuery] = useState("");
  const [searchResults, setSearchResults] = useState<SearchResult[]>(mockResults);
  const [selectedIndex, setSelectedIndex] = useState(0);
  const [status, setStatus] = useState<IndexStatus>(browserStatus);
  const [plugins, setPlugins] = useState<PluginInfo[]>(mockPlugins);
  const [health, setHealth] = useState<AppHealth | null>(null);
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [activePlugin, setActivePlugin] = useState<PluginInfo | null>(null);
  const [pendingPluginEvent, setPendingPluginEvent] = useState<PluginFrontendEvent | null>(null);
  const [toast, setToast] = useState<string | null>(null);
  const [isRefreshing, setIsRefreshing] = useState(false);
  const [availableUpdate, setAvailableUpdate] = useState<Update | null>(null);
  const [updatePhase, setUpdatePhase] = useState<UpdatePhase>("idle");
  const [updateProgress, setUpdateProgress] = useState<UpdateProgress>({ received: 0 });
  const [updateError, setUpdateError] = useState<string | null>(null);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [autostartEnabled, setAutostartEnabled] = useState<boolean | null>(null);
  const [isUpdatingAutostart, setIsUpdatingAutostart] = useState(false);
  const [autostartError, setAutostartError] = useState<string | null>(null);

  const results = useMemo(() => {
    const seen = new Set<string>();
    return [...pluginCommandResults(plugins, query), ...searchResults].filter((result) => {
      const identity = result.pluginId && result.commandId
        ? `${result.pluginId}:${result.commandId}`
        : result.id;
      if (seen.has(identity)) {
        return false;
      }
      seen.add(identity);
      return true;
    });
  }, [plugins, query, searchResults]);
  const selectedResult = results[selectedIndex];
  const rootSummary = useMemo(
    () =>
      status.roots.length
        ? status.roots.slice(0, 2).join(" · ") +
          (status.roots.length > 2 ? " · …" : "")
        : "选定位置将由 Rust 核心建立索引",
    [status.roots],
  );

  const showToast = useCallback((message: string) => {
    setToast(message);
    window.setTimeout(() => setToast((current) => (current === message ? null : current)), 3600);
  }, []);

  const refreshStatus = useCallback(async () => {
    if (!isDesktop()) {
      return;
    }

    try {
      const [nextStatus, nextHealth] = await Promise.all([
        command<IndexStatus>("get_index_status"),
        command<AppHealth>("get_app_health"),
      ]);
      setStatus(nextStatus);
      setHealth(nextHealth);
      setAutostartEnabled(nextHealth.autostart ?? null);
    } catch (error) {
      setStatus((current) => ({
        ...current,
        phase: "error",
        message: error instanceof Error ? error.message : "无法读取索引状态",
      }));
    }
  }, []);

  const refreshPlugins = useCallback(async () => {
    if (!isDesktop()) {
      return;
    }

    try {
      setPlugins(await command<PluginInfo[]>("list_plugins"));
    } catch (error) {
      showToast(error instanceof Error ? error.message : "无法读取插件列表。");
    }
  }, [showToast]);

  const requestSearch = useCallback(async (nextQuery: string) => {
    if (!isDesktop()) {
      setSearchResults(filterPreviewResults(nextQuery));
      return;
    }

    try {
      const next = await command<SearchResult[]>("search_entries", {
        query: nextQuery,
        limit: 12,
      });
      setSearchResults(next);
    } catch (error) {
      showToast(error instanceof Error ? error.message : "搜索引擎暂不可用。");
    }
  }, [showToast]);

  useEffect(() => {
    const timer = window.setTimeout(() => {
      void requestSearch(query);
    }, query ? 55 : 0);

    return () => window.clearTimeout(timer);
  }, [query, requestSearch]);

  useEffect(() => {
    setSelectedIndex(0);
  }, [results]);

  useEffect(() => {
    if (booted.current) {
      return;
    }
    booted.current = true;

    let unlisten: () => void = () => {};
    let interval: number | undefined;
    let disposed = false;
    const start = async () => {
      if (!isDesktop()) {
        return;
      }

      await Promise.all([refreshStatus(), refreshPlugins()]);
      try {
        unlisten = await onFocusSearch(() => {
          inputRef.current?.focus();
          inputRef.current?.select();
        });
      } catch {
        // A manual click remains available if a global shortcut is unavailable.
      }

      void command<void>("index_default_roots")
        .then(() => refreshStatus())
        .catch((error) =>
          showToast(error instanceof Error ? error.message : "无法启动索引。"),
        );

      interval = window.setInterval(() => void refreshStatus(), 1800);
      try {
        setUpdatePhase("checking");
        const update = await check();
        if (disposed) {
          void update?.close().catch(() => undefined);
          return;
        }

        if (update) {
          const previousUpdate = updateRef.current;
          updateRef.current = update;
          setAvailableUpdate(update);
          setUpdatePhase("available");
          void previousUpdate?.close().catch(() => undefined);
        } else {
          setUpdatePhase("idle");
        }
      } catch {
        // Release builds get a signed updater endpoint; development builds intentionally do not.
        if (!disposed) {
          setUpdatePhase("idle");
        }
      }
    };

    void start();
    return () => {
      disposed = true;
      unlisten();
      if (interval !== undefined) {
        window.clearInterval(interval);
      }
      const retainedUpdate = updateRef.current;
      updateRef.current = null;
      void retainedUpdate?.close().catch(() => undefined);
    };
  }, [refreshPlugins, refreshStatus, showToast]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLocaleLowerCase() === "k") {
        event.preventDefault();
        inputRef.current?.focus();
        inputRef.current?.select();
      }
      if (event.key === "Escape") {
        if (activePlugin) {
          setActivePlugin(null);
          setPendingPluginEvent(null);
        } else if (settingsOpen) {
          setSettingsOpen(false);
        } else if (drawerOpen) {
          setDrawerOpen(false);
        } else {
          setQuery("");
          inputRef.current?.blur();
        }
      }
      if (event.key === "ArrowDown" && document.activeElement === inputRef.current) {
        event.preventDefault();
        setSelectedIndex((current) => Math.min(current + 1, results.length - 1));
      }
      if (event.key === "ArrowUp" && document.activeElement === inputRef.current) {
        event.preventDefault();
        setSelectedIndex((current) => Math.max(current - 1, 0));
      }
    };

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [activePlugin, drawerOpen, results.length, settingsOpen]);

  const refreshIndex = async () => {
    if (!isDesktop()) {
      showToast("桌面版会在后台索引你选择的位置。");
      return;
    }

    setIsRefreshing(true);
    try {
      await command<void>("index_default_roots");
      await refreshStatus();
      showToast("已开始增量索引。你可以立即继续搜索。");
    } catch (error) {
      showToast(error instanceof Error ? error.message : "无法刷新索引。");
    } finally {
      setIsRefreshing(false);
    }
  };

  const installUpdate = async () => {
    if (!isDesktop()) {
      showToast("浏览器预览不会下载更新；请在 iHub 桌面端执行此操作。");
      return;
    }

    const update = updateRef.current ?? availableUpdate;
    if (!update) {
      showToast("当前没有可安装的更新。");
      return;
    }

    setUpdateError(null);
    setUpdateProgress({ received: 0 });
    setUpdatePhase("downloading");

    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          setUpdateProgress({
            received: 0,
            total: event.data.contentLength,
          });
          setUpdatePhase("downloading");
        } else if (event.event === "Progress") {
          setUpdateProgress((current) => ({
            ...current,
            received: current.received + event.data.chunkLength,
          }));
        } else {
          setUpdatePhase("installing");
        }
      });

      if (updateRef.current === update) {
        updateRef.current = null;
      }
      setAvailableUpdate(null);
      setUpdatePhase("installed");
      void update.close().catch(() => undefined);
      showToast("更新已安装；重启 iHub 后生效。");
    } catch (error) {
      const message = error instanceof Error ? error.message : "更新安装失败。";
      setUpdateError(message);
      setUpdatePhase("error");
      showToast(message);
    }
  };

  const toggleAutostart = async () => {
    if (!isDesktop()) {
      showToast("浏览器预览不会修改开机自启动；请在 iHub 桌面端设置。");
      return;
    }

    const nextEnabled = !(autostartEnabled ?? health?.autostart ?? false);
    setAutostartError(null);
    setIsUpdatingAutostart(true);
    try {
      const result = await command<AutostartStatus>("set_autostart", {
        enabled: nextEnabled,
      });
      setAutostartEnabled(result.enabled);
      setHealth((current) =>
        current ? { ...current, autostart: result.enabled } : current,
      );
      showToast(result.enabled ? "开机自启动已启用。" : "开机自启动已关闭。");
    } catch (error) {
      const message = error instanceof Error ? error.message : "无法更新开机自启动设置。";
      setAutostartError(message);
      showToast(message);
    } finally {
      setIsUpdatingAutostart(false);
    }
  };

  const updateActionLabel = (() => {
    if (updatePhase === "downloading") {
      if (updateProgress.total) {
        return `下载 ${Math.min(100, Math.round((updateProgress.received / updateProgress.total) * 100))}%`;
      }
      return "正在下载";
    }
    if (updatePhase === "installing") {
      return "正在安装";
    }
    if (updatePhase === "installed") {
      return "重启生效";
    }
    if (updatePhase === "error") {
      return "重试更新";
    }
    return availableUpdate ? `更新至 v${availableUpdate.version}` : "更新可用";
  })();

  const hasUpdateAction = Boolean(availableUpdate) && !["downloading", "installing", "installed"].includes(updatePhase);
  const autostartIsEnabled = autostartEnabled ?? health?.autostart ?? false;

  const activateResult = async (result?: SearchResult) => {
    if (!result) {
      return;
    }

    if (result.commandId === "ihub.index.default") {
      await refreshIndex();
      return;
    }

    if (!isDesktop()) {
      showToast("这是界面预览；在 iHub 桌面端中执行此操作。");
      return;
    }

    try {
      if ((result.kind === "file" || result.kind === "folder") && result.path) {
        await command<void>("open_path", { path: result.path });
      } else if (result.pluginId && result.commandId) {
        const plugin = plugins.find((item) => item.id === result.pluginId);
        if (plugin?.frontendEntry && !plugin.hasNativeWorker) {
          setDrawerOpen(false);
          setPendingPluginEvent(createFrontendCommandEvent(plugin.id, result.commandId));
          setActivePlugin(plugin);
          return;
        }
        if (!plugin?.hasNativeWorker) {
          showToast("该插件没有可运行的原生 worker 或前端命令入口。");
          return;
        }
        const approvalKey = `${plugin.id}@${plugin.version}`;
        if (!approvedNativePlugins.current.has(approvalKey)) {
          const approved = window.confirm(
            `“${plugin.name}” 将启动本机二进制 worker。\n\n原生插件不受沙箱限制，只应运行你信任的发布者。是否继续？`,
          );
          if (!approved) {
            showToast("已取消启动原生插件。你可以在确认来源后再次执行。");
            return;
          }
          approvedNativePlugins.current.add(approvalKey);
        }
        await command<void>("run_plugin_command", {
          pluginId: result.pluginId,
          commandId: result.commandId,
        });
        showToast("插件已交给独立 worker 执行。");
      }
    } catch (error) {
      showToast(error instanceof Error ? error.message : "无法执行该项目。");
    }
  };

  const onInputKeyDown = (event: ReactKeyboardEvent<HTMLInputElement>) => {
    if (event.key === "Enter") {
      event.preventDefault();
      void activateResult(selectedResult);
    }
  };

  const onResultClick = (event: MouseEvent<HTMLButtonElement>, result: SearchResult) => {
    event.currentTarget.focus({ preventScroll: true });
    void activateResult(result);
  };

  return (
    <main className="app-shell">
      <div className="ambient ambient--one" />
      <div className="ambient ambient--two" />
      <header className="topbar">
        <button
          aria-label="Focus search"
          className="brand"
          onClick={() => inputRef.current?.focus()}
        >
          <span className="brand-mark">
            <span />
            <span />
          </span>
          <span>iHub</span>
        </button>
        <div className="topbar__actions">
          {availableUpdate || updatePhase === "installed" ? (
            <button
              aria-live="polite"
              className={"update-button is-" + updatePhase}
              disabled={!hasUpdateAction}
              onClick={() => void installUpdate()}
              title={
                updatePhase === "installed"
                  ? "更新已安装，重启 iHub 后生效"
                  : availableUpdate?.body ?? "下载并安装可用更新"
              }
              type="button"
            >
              {updatePhase === "downloading" || updatePhase === "installing" ? (
                <LoaderCircle className="spin" size={14} />
              ) : updatePhase === "installed" ? (
                <Check size={14} />
              ) : (
                <Download size={14} />
              )}
              <span>{updateActionLabel}</span>
            </button>
          ) : null}
          <button className="quiet-button" onClick={() => setDrawerOpen(true)}>
            <GitBranch size={15} />
            插件
          </button>
          <button
            aria-expanded={settingsOpen}
            aria-label="打开设置"
            className="icon-button"
            onClick={() => setSettingsOpen((current) => !current)}
            type="button"
          >
            <Settings2 size={17} />
          </button>
        </div>
      </header>

      <AnimatePresence>
        {settingsOpen ? (
          <>
            <motion.button
              aria-label="关闭设置"
              className="settings-scrim"
              initial={{ opacity: 0 }}
              animate={{ opacity: 1 }}
              exit={{ opacity: 0 }}
              onClick={() => setSettingsOpen(false)}
              type="button"
            />
            <motion.aside
              aria-labelledby="settings-title"
              className="settings-panel"
              initial={prefersReducedMotion ? false : { opacity: 0, y: -8, scale: 0.98 }}
              animate={prefersReducedMotion ? undefined : { opacity: 1, y: 0, scale: 1 }}
              exit={prefersReducedMotion ? undefined : { opacity: 0, y: -6, scale: 0.985 }}
              role="dialog"
              transition={{ duration: 0.18, ease: [0.16, 1, 0.3, 1] }}
            >
              <div className="settings-panel__header">
                <div>
                  <span>APPLICATION</span>
                  <h2 id="settings-title">偏好设置</h2>
                </div>
                <button
                  aria-label="关闭设置"
                  className="icon-button"
                  onClick={() => setSettingsOpen(false)}
                  type="button"
                >
                  <X size={16} />
                </button>
              </div>

              <section className="settings-section" aria-labelledby="updates-title">
                <div className="settings-section__icon">
                  {updatePhase === "installed" ? <Check size={16} /> : <Download size={16} />}
                </div>
                <div className="settings-section__copy">
                  <h3 id="updates-title">自动更新</h3>
                  <p>
                    {!isDesktop()
                      ? "浏览器预览不会查询或安装发行更新。"
                      : updatePhase === "installed"
                        ? "已安装，重启 iHub 后生效。"
                        : availableUpdate
                          ? `发现 v${availableUpdate.version}，签名更新将下载后安装。`
                          : updatePhase === "checking"
                            ? "正在检查签名更新…"
                            : "启动时会检查已签名的发行更新。"}
                  </p>
                  {availableUpdate ? (
                    <button
                      className="settings-action"
                      disabled={!hasUpdateAction}
                      onClick={() => void installUpdate()}
                      type="button"
                    >
                      {updatePhase === "downloading" || updatePhase === "installing" ? (
                        <LoaderCircle className="spin" size={14} />
                      ) : (
                        <Download size={14} />
                      )}
                      {updateActionLabel}
                    </button>
                  ) : null}
                  {updatePhase === "downloading" ? (
                    <div className="update-progress" aria-label={updateActionLabel}>
                      <span
                        style={{
                          width: updateProgress.total
                            ? `${Math.min(100, (updateProgress.received / updateProgress.total) * 100)}%`
                            : "18%",
                        }}
                      />
                    </div>
                  ) : null}
                  {updateError ? (
                    <p className="settings-error" role="alert">
                      <CircleAlert size={13} />
                      {updateError}
                    </p>
                  ) : null}
                </div>
              </section>

              <section className="settings-section" aria-labelledby="autostart-title">
                <div className="settings-section__icon">
                  <Power size={16} />
                </div>
                <div className="settings-section__copy">
                  <h3 id="autostart-title">开机自启动</h3>
                  <p>
                    {!isDesktop()
                      ? "浏览器预览仅展示此选项，不会更改系统设置。"
                      : isUpdatingAutostart
                        ? "正在更新系统启动项…"
                        : autostartEnabled === null
                        ? "正在读取系统启动项…"
                        : autostartIsEnabled
                          ? "已启用：登录后 iHub 会在后台就绪。"
                          : "已关闭：需要时从应用程序中手动启动。"}
                  </p>
                  {autostartError ? (
                    <p className="settings-error" role="alert">
                      <CircleAlert size={13} />
                      {autostartError}
                    </p>
                  ) : null}
                </div>
                <button
                  aria-label={autostartIsEnabled ? "关闭开机自启动" : "启用开机自启动"}
                  aria-busy={isUpdatingAutostart}
                  aria-pressed={autostartIsEnabled}
                  className={"settings-switch" + (autostartIsEnabled ? " is-on" : "")}
                  disabled={isUpdatingAutostart}
                  onClick={() => void toggleAutostart()}
                  type="button"
                >
                  <span />
                </button>
              </section>

              <p className="settings-panel__meta">
                {isDesktop() && health ? `iHub ${health.version} · ${health.platform}` : "iHub 浏览器预览"}
              </p>
            </motion.aside>
          </>
        ) : null}
      </AnimatePresence>

      <section className="command-stage" aria-labelledby="command-title">
        <motion.div
          className="stage-intro"
          initial={prefersReducedMotion ? false : { opacity: 0, y: 14 }}
          animate={prefersReducedMotion ? undefined : { opacity: 1, y: 0 }}
          transition={{ duration: 0.55, ease: [0.16, 1, 0.3, 1] }}
        >
          <p className="eyebrow">
            <Sparkles size={13} />
            YOUR LOCAL COMMAND SPACE
          </p>
          <h1 id="command-title">
            <BlurText delay={0.08} text="Find. Launch. Extend." />
          </h1>
          <p>
            文件、内容和可信插件命令，都从一个不打断思路的入口开始。
          </p>
        </motion.div>

        <motion.div
          className="search-plane"
          initial={prefersReducedMotion ? false : { opacity: 0, scale: 0.985, y: 12 }}
          animate={prefersReducedMotion ? undefined : { opacity: 1, scale: 1, y: 0 }}
          transition={{ delay: 0.12, duration: 0.58, ease: [0.16, 1, 0.3, 1] }}
        >
          <div className="search-line">
            <Search aria-hidden="true" className="search-line__icon" size={21} />
            <input
              aria-activedescendant={selectedResult?.id}
              aria-controls="search-results"
              aria-label="Search files and commands"
              autoComplete="off"
              autoFocus
              onChange={(event) => setQuery(event.target.value)}
              onKeyDown={onInputKeyDown}
              placeholder="搜索文件、内容、动作或插件…"
              ref={inputRef}
              role="combobox"
              value={query}
            />
            <kbd>{platformShortcut()}</kbd>
          </div>

          <div className="result-area">
            <div className="result-area__caption">
              <span>{query ? "最佳匹配" : "从这里开始"}</span>
              <span>{results.length} 项</span>
            </div>
            <div aria-label="Search results" className="result-list" id="search-results" role="listbox">
              <AnimatePresence initial={false} mode="popLayout">
                {results.length ? (
                  results.map((result, index) => (
                    <motion.button
                      aria-selected={index === selectedIndex}
                      className={"result-row" + (index === selectedIndex ? " is-selected" : "")}
                      id={result.id}
                      initial={prefersReducedMotion ? false : { opacity: 0, y: 6 }}
                      animate={prefersReducedMotion ? undefined : { opacity: 1, y: 0 }}
                      exit={prefersReducedMotion ? undefined : { opacity: 0, y: -4 }}
                      key={result.id}
                      layout="position"
                      onClick={(event) => onResultClick(event, result)}
                      onMouseEnter={() => setSelectedIndex(index)}
                      role="option"
                      transition={{ duration: 0.2, ease: "easeOut" }}
                    >
                      <ResultIcon kind={result.kind} />
                      <span className="result-row__text">
                        <strong>{result.name}</strong>
                        <small>{result.metadata ?? result.path ?? result.kind}</small>
                      </span>
                      <span className="result-row__action">
                        <span>{index === selectedIndex ? "打开" : result.kind}</span>
                        <ChevronRight size={16} />
                      </span>
                    </motion.button>
                  ))
                ) : (
                  <motion.div
                    className="empty-state"
                    initial={{ opacity: 0 }}
                    animate={{ opacity: 1 }}
                  >
                    <Search size={20} />
                    <span>没有命中。尝试更短的名称或安装一个插件。</span>
                  </motion.div>
                )}
              </AnimatePresence>
            </div>
          </div>

          <div className="search-plane__footer">
            <span>
              <CommandIcon size={14} />
              <b>↑↓</b> 选择 <b>↵</b> 打开
            </span>
            <span>
              <Zap size={14} />
              Rust local index · plugin workers on demand
            </span>
          </div>
        </motion.div>
      </section>

      <section className="signal-strip" aria-label="iHub status">
        <div className="signal-strip__item">
          <span className={"live-indicator is-" + status.phase} />
          <div>
            <small>LOCAL INDEX</small>
            <strong>{statusLabel(status)}</strong>
          </div>
        </div>
        <div className="signal-strip__metric">
          <HardDrive size={16} />
          <strong>{new Intl.NumberFormat().format(status.indexedFiles)}</strong>
          <span>项目</span>
        </div>
        <div className="signal-strip__root" title={rootSummary}>
          <Activity size={16} />
          <span>{status.message ?? rootSummary}</span>
        </div>
        <button
          className="refresh-button"
          disabled={isRefreshing}
          onClick={() => void refreshIndex()}
        >
          {isRefreshing ? <LoaderCircle className="spin" size={16} /> : <RefreshCw size={16} />}
          刷新索引
        </button>
      </section>

      <section className="bottom-grid" aria-label="Quick actions">
        <button className="extension-invite" onClick={() => setDrawerOpen(true)}>
          <span className="extension-invite__mark">
            <Plus size={19} />
          </span>
          <span>
            <small>DECENTRALIZED PLUGINS</small>
            <strong>从 GitHub 导入你的下一项能力</strong>
          </span>
          <ArrowRight size={18} />
        </button>
        <div className="trust-note">
          <Check size={15} />
          <span>
            插件前端经 iHub Bridge 调用；原生二进制只在你明确确认后启动。
          </span>
        </div>
      </section>

      <PluginDrawer
        onClose={() => setDrawerOpen(false)}
        onOpenFrontend={(plugin) => {
          setDrawerOpen(false);
          setPendingPluginEvent(null);
          setActivePlugin(plugin);
        }}
        onPluginsChanged={setPlugins}
        onToast={showToast}
        open={drawerOpen}
        plugins={plugins}
      />
      <PluginFrontendFrame
        onClose={() => {
          setActivePlugin(null);
          setPendingPluginEvent(null);
        }}
        onPendingEventHandled={(eventId) => {
          setPendingPluginEvent((current) => (current?.id === eventId ? null : current));
        }}
        onToast={showToast}
        pendingEvent={pendingPluginEvent}
        plugin={activePlugin}
      />

      <AnimatePresence>
        {toast ? (
          <motion.div
            className="toast"
            initial={{ opacity: 0, y: 12, scale: 0.98 }}
            animate={{ opacity: 1, y: 0, scale: 1 }}
            exit={{ opacity: 0, y: 8, scale: 0.98 }}
            role="status"
          >
            {toast}
          </motion.div>
        ) : null}
      </AnimatePresence>
    </main>
  );
}
