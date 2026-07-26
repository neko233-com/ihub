import { AnimatePresence, motion } from "motion/react";
import {
  ArrowUpRight,
  CheckCircle2,
  GitBranch,
  LoaderCircle,
  Plus,
  Puzzle,
  ShieldAlert,
  X,
} from "lucide-react";
import { useState } from "react";
import { command, isDesktop } from "../lib/desktop";
import type { PluginInfo } from "../lib/types";

interface PluginDrawerProps {
  open: boolean;
  plugins: PluginInfo[];
  onClose: () => void;
  onOpenFrontend: (plugin: PluginInfo) => void;
  onPluginsChanged: (plugins: PluginInfo[]) => void;
  onToast: (message: string) => void;
}

function sourceIsPlausible(value: string) {
  return (
    value.startsWith("github:") ||
    /^[A-Za-z0-9][A-Za-z0-9._-]*\/[A-Za-z0-9][A-Za-z0-9._-]*$/.test(value.trim()) ||
    /^https:\/\/github\.com\/[^/]+\/[^/]+/i.test(value.trim())
  );
}

function pluginCommandCount(plugin: PluginInfo) {
  if (typeof plugin.commandCount === "number") {
    return plugin.commandCount;
  }
  if (typeof plugin.commands === "number") {
    return plugin.commands;
  }
  return plugin.commands?.length ?? 0;
}

export function PluginDrawer({
  open,
  plugins,
  onClose,
  onOpenFrontend,
  onPluginsChanged,
  onToast,
}: PluginDrawerProps) {
  const [source, setSource] = useState("");
  const [installing, setInstalling] = useState(false);

  const install = async () => {
    const normalized = source.trim();
    if (!sourceIsPlausible(normalized)) {
      onToast("请输入 owner/repo、github:owner/repo 或完整 GitHub 仓库链接。");
      return;
    }

    if (!isDesktop()) {
      onToast("浏览器预览不会下载或执行第三方插件。");
      return;
    }

    setInstalling(true);
    try {
      await command<PluginInfo>("install_plugin_from_git", { source: normalized });
      const next = await command<PluginInfo[]>("list_plugins");
      onPluginsChanged(next);
      setSource("");
      onToast("插件已安装。二进制插件会在首次执行前再次要求确认。");
    } catch (error) {
      onToast(error instanceof Error ? error.message : "插件安装失败。");
    } finally {
      setInstalling(false);
    }
  };

  return (
    <AnimatePresence>
      {open ? (
        <>
          <motion.button
            aria-label="Close plugin drawer"
            className="drawer-scrim"
            initial={{ opacity: 0 }}
            animate={{ opacity: 1 }}
            exit={{ opacity: 0 }}
            onClick={onClose}
          />
          <motion.aside
            aria-label="Plugin manager"
            className="plugin-drawer"
            initial={{ opacity: 0, x: 28 }}
            animate={{ opacity: 1, x: 0 }}
            exit={{ opacity: 0, x: 28 }}
            transition={{ type: "spring", stiffness: 420, damping: 36 }}
          >
            <div className="drawer-header">
              <div>
                <p className="eyebrow">EXTENSIONS</p>
                <h2>插件空间</h2>
              </div>
              <button aria-label="Close" className="icon-button" onClick={onClose}>
                <X size={18} />
              </button>
            </div>

            <div className="plugin-import">
              <GitBranch aria-hidden="true" size={18} />
              <div>
                <label htmlFor="plugin-source">从 GitHub 导入</label>
                <p>锁定 commit；只安装已构建的插件产物。</p>
              </div>
              <div className="plugin-import__input">
                <input
                  autoCapitalize="none"
                  autoCorrect="off"
                  id="plugin-source"
                  onChange={(event) => setSource(event.target.value)}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") {
                      void install();
                    }
                  }}
                  placeholder="owner/ihub-plugin-name"
                  spellCheck="false"
                  value={source}
                />
                <button
                  aria-label="Install plugin"
                  className="accent-button accent-button--compact"
                  disabled={installing}
                  onClick={() => void install()}
                >
                  {installing ? (
                    <LoaderCircle className="spin" size={16} />
                  ) : (
                    <Plus size={16} />
                  )}
                </button>
              </div>
            </div>

            <div className="plugin-risk">
              <ShieldAlert aria-hidden="true" size={16} />
              <span>
                原生 worker 不受沙箱限制。只安装你信任的发布者，并仔细核对权限和哈希。
              </span>
            </div>

            <div className="plugin-list">
              <div className="section-caption">
                <span>已安装</span>
                <span>{plugins.length}</span>
              </div>
              {plugins.map((plugin) => (
                <article className="plugin-row" key={plugin.id}>
                  <span className="plugin-row__glyph">
                    <Puzzle size={16} strokeWidth={1.75} />
                  </span>
                  <div className="plugin-row__content">
                    <div className="plugin-row__title">
                      <strong>{plugin.name}</strong>
                      <span>v{plugin.version}</span>
                    </div>
                    <p>{plugin.description ?? plugin.id}</p>
                    <small>
                      {plugin.hasNativeWorker ? "Native worker · " : ""}
                      {pluginCommandCount(plugin)} commands
                    </small>
                    {plugin.frontendEntry ? (
                      <button
                        className="plugin-row__open"
                        onClick={() => onOpenFrontend(plugin)}
                        type="button"
                      >
                        打开界面 <ArrowUpRight size={13} />
                      </button>
                    ) : null}
                  </div>
                  {plugin.enabled !== false ? (
                    <CheckCircle2
                      aria-label="Enabled"
                      className="plugin-row__enabled"
                      size={17}
                    />
                  ) : null}
                </article>
              ))}
            </div>

            <a
              className="drawer-doc-link"
              href="https://github.com/neko233-com/ihub/blob/main/docs/PLUGIN_DEVELOPMENT.md"
              rel="noreferrer"
              target="_blank"
            >
              开发第一个插件 <ArrowUpRight size={15} />
            </a>
          </motion.aside>
        </>
      ) : null}
    </AnimatePresence>
  );
}
