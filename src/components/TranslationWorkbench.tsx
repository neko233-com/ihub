import {
  ArrowLeftRight,
  Check,
  Copy,
  Languages,
  PackagePlus,
  Trash2,
  WifiOff,
  X,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState } from "react";
import {
  BUILTIN_ZH_EN_PACK,
  detectOfflineLanguage,
  MAX_OFFLINE_PACK_BYTES,
  parseOfflineTranslationPack,
  translateOffline,
  type OfflineTranslationPack,
} from "../lib/offline-translation";

interface TranslationWorkbenchProps {
  input: string;
  onClose: () => void;
  onCopy: (value: string, label: string) => Promise<void> | void;
  onInputChange: (value: string) => void;
  onStartWindowDrag?: () => void;
  onToast: (message: string) => void;
}

const packsStorageKey = "ihub.offline-translation.packs.v1";
const maximumCustomPacks = 8;
const maximumStoredPackBytes = 2 * 1024 * 1024;

function serializedByteLength(value: string): number {
  return new TextEncoder().encode(value).byteLength;
}

function restorePacks(): OfflineTranslationPack[] {
  try {
    const raw = window.localStorage.getItem(packsStorageKey);
    if (!raw || serializedByteLength(raw) > maximumStoredPackBytes) return [];
    const values: unknown = JSON.parse(raw);
    if (!Array.isArray(values)) return [];
    const restored: OfflineTranslationPack[] = [];
    const restoredIds = new Set<string>();
    for (const value of values.slice(0, maximumCustomPacks)) {
      try {
        const pack = parseOfflineTranslationPack(JSON.stringify(value));
        if (!restoredIds.has(pack.id)) {
          restored.push(pack);
          restoredIds.add(pack.id);
        }
      } catch {
        // Invalid persisted packs are ignored independently so one damaged
        // entry cannot prevent the remaining local packs from loading.
      }
    }
    return restored;
  } catch {
    return [];
  }
}

function languageLabel(code: string): string {
  const fixed: Record<string, string> = {
    auto: "自动检测",
    "zh-CN": "中文（简体）",
    en: "English",
    ja: "日本語",
    ko: "한국어",
    fr: "Français",
    de: "Deutsch",
    es: "Español",
  };
  if (fixed[code]) return fixed[code];
  try {
    return new Intl.DisplayNames(["zh-CN"], { type: "language" }).of(code) ?? code;
  } catch {
    return code;
  }
}

export function TranslationWorkbench({
  input,
  onClose,
  onCopy,
  onInputChange,
  onStartWindowDrag,
  onToast,
}: TranslationWorkbenchProps) {
  const [sourceLanguage, setSourceLanguage] = useState("auto");
  const [targetLanguage, setTargetLanguage] = useState("en");
  const [packs, setPacks] = useState<OfflineTranslationPack[]>(restorePacks);
  const [showPacks, setShowPacks] = useState(false);
  const [swapEpoch, setSwapEpoch] = useState(0);
  const packInputRef = useRef<HTMLInputElement | null>(null);
  const languageCodes = useMemo(() => Array.from(new Set([
    "zh-CN",
    "en",
    ...packs.flatMap((pack) => [pack.source, pack.target]),
  ])), [packs]);
  const detectedLanguage = useMemo(() => input.trim()
    ? detectOfflineLanguage(input, packs)
    : "zh-CN", [input, packs]);
  const resolvedSource = sourceLanguage === "auto" ? detectedLanguage : sourceLanguage;
  const translation = useMemo(() => {
    if (!input.trim()) return { result: null, error: null };
    try {
      return {
        result: translateOffline(input, resolvedSource, targetLanguage, packs),
        error: null,
      };
    } catch (error) {
      return {
        result: null,
        error: error instanceof Error ? error.message : "本地翻译失败。",
      };
    }
  }, [input, packs, resolvedSource, targetLanguage]);

  useEffect(() => {
    if (sourceLanguage !== "auto" || !input.trim() || detectedLanguage !== targetLanguage) return;
    if (detectedLanguage === "en") setTargetLanguage("zh-CN");
    else if (detectedLanguage === "zh-CN") setTargetLanguage("en");
  }, [detectedLanguage, input, sourceLanguage, targetLanguage]);

  const savePacks = (next: OfflineTranslationPack[]) => {
    const serialized = JSON.stringify(next);
    if (serializedByteLength(serialized) > maximumStoredPackBytes) {
      onToast("本地语言包总量不能超过 2 MiB。");
      return;
    }
    try {
      window.localStorage.setItem(packsStorageKey, serialized);
      setPacks(next);
    } catch {
      onToast("浏览器本地存储空间不足；未保存语言包。");
    }
  };

  const importPack = async (file: File) => {
    try {
      if (file.size > MAX_OFFLINE_PACK_BYTES) {
        throw new Error("离线语言包不能超过 1 MiB。");
      }
      const pack = parseOfflineTranslationPack(await file.text());
      const next = [pack, ...packs.filter((candidate) => candidate.id !== pack.id)]
        .slice(0, maximumCustomPacks);
      savePacks(next);
      setShowPacks(true);
      onToast(`已在本机导入 ${pack.name}。`);
    } catch (error) {
      onToast(error instanceof Error ? error.message : "无法导入本地语言包。");
    } finally {
      if (packInputRef.current) packInputRef.current.value = "";
    }
  };

  const swapLanguages = () => {
    const nextSource = targetLanguage;
    const nextTarget = resolvedSource;
    setSwapEpoch((current) => current + 1);
    setSourceLanguage(nextSource);
    setTargetLanguage(nextTarget);
    if (translation.result?.text) onInputChange(translation.result.text);
  };

  return (
    <section aria-label="本地离线翻译工作台" className="translation-workbench">
      <header
        className="translation-workbench__header"
        data-tauri-drag-region="true"
        onMouseDown={(event) => {
          if (event.button === 0 && event.target === event.currentTarget) onStartWindowDrag?.();
        }}
      >
        <div className="translation-workbench__identity">
          <span><Languages size={18} /></span>
          <div><strong id="translation-workbench-title">离线翻译</strong><small>Local Translate</small></div>
        </div>
        <div className="translation-workbench__privacy"><WifiOff size={14} /><span>本地处理 · 网络请求 0</span></div>
        <div className="translation-workbench__header-actions">
          <button onClick={() => setShowPacks((current) => !current)} type="button"><PackagePlus size={15} />语言包</button>
          <button aria-label="关闭离线翻译" onClick={onClose} type="button"><X size={17} /></button>
        </div>
      </header>

      <div className="translation-workbench__language-bar">
        <label>
          <span>原文</span>
          <select aria-label="原文语言" onChange={(event) => setSourceLanguage(event.target.value)} value={sourceLanguage}>
            <option value="auto">自动检测</option>
            {languageCodes.map((code) => <option key={code} value={code}>{languageLabel(code)}</option>)}
          </select>
        </label>
        <button aria-label="交换翻译语言" data-swap-parity={swapEpoch % 2 ? "odd" : "even"} onClick={swapLanguages} type="button"><ArrowLeftRight size={16} /></button>
        <label>
          <span>译文</span>
          <select aria-label="译文语言" onChange={(event) => setTargetLanguage(event.target.value)} value={targetLanguage}>
            {languageCodes.map((code) => <option key={code} value={code}>{languageLabel(code)}</option>)}
          </select>
        </label>
      </div>

      <main className="translation-workbench__panes">
        <section className="translation-workbench__pane translation-workbench__pane--source">
          <div><span>{sourceLanguage === "auto" ? `检测：${languageLabel(detectedLanguage)}` : languageLabel(sourceLanguage)}</span><small>{input.length.toLocaleString()} / 32,768</small></div>
          <textarea
            aria-label="待翻译文本"
            autoFocus
            maxLength={32_768}
            onChange={(event) => onInputChange(event.target.value)}
            onKeyDown={(event) => {
              if ((event.ctrlKey || event.metaKey) && event.key === "Enter" && translation.result?.text) {
                event.preventDefault();
                void onCopy(translation.result.text, "译文");
              }
            }}
            placeholder="输入或粘贴文本；内容不会离开当前设备。"
            spellCheck={false}
            value={input}
          />
          <div className="translation-workbench__pane-actions">
            <button disabled={!input} onClick={() => onInputChange("")} type="button">清空</button>
            <button
              onClick={() => void navigator.clipboard?.readText()
                .then((text) => onInputChange(text.slice(0, 32_768)))
                .catch(() => onToast("无法读取剪贴板；请直接粘贴文本。"))}
              type="button"
            >粘贴</button>
          </div>
        </section>

        <section className="translation-workbench__pane translation-workbench__pane--target">
          <div>
            <span>{languageLabel(targetLanguage)}</span>
            {translation.result ? <small>词典覆盖 {Math.round(translation.result.coverage * 100)}%</small> : null}
          </div>
          <div
            aria-live="polite"
            className={`translation-workbench__output${translation.error ? " is-error" : ""}`}
            key={`${translation.result?.packIds.join(":") ?? "empty"}:${translation.result?.text ?? translation.error ?? ""}`}
          >
            {translation.error
              ? translation.error
              : translation.result?.text || "译文会在本机即时显示。"}
          </div>
          <div className="translation-workbench__pane-actions">
            <span>{translation.result
              ? translation.result.pivotLanguage
                ? `经 English 枢轴 · ${translation.result.packIds.length} 个包`
                : translation.result.packId === BUILTIN_ZH_EN_PACK.id
                  ? "内置中英基础包 v1"
                  : `${translation.result.packIds.length} 个本地包`
              : "等待输入"}</span>
            <button disabled={!translation.result?.text} onClick={() => void onCopy(translation.result?.text ?? "", "译文")} type="button"><Copy size={14} />复制译文</button>
          </div>
        </section>
      </main>

      <footer className="translation-workbench__footer">
        <div><Check size={14} /><span>默认中英词典路由已随应用安装</span></div>
        <span>Ctrl / ⌘ + Enter 复制译文</span>
        {translation.result?.unknownSegments.length ? (
          <span title={translation.result.unknownSegments.join("、")}>未覆盖片段 {translation.result.unknownSegments.length} 个</span>
        ) : <span>无云端回退</span>}
      </footer>

      {showPacks ? (
        <aside aria-label="本地语言包" className="translation-workbench__packs">
          <div><strong>本地语言包</strong><button aria-label="关闭语言包面板" onClick={() => setShowPacks(false)} type="button"><X size={16} /></button></div>
          <p>默认中英包随应用提供。自定义包优先覆盖内置术语，也可经 English 枢轴连接其他语言；不会下载、上传或执行包内代码。</p>
          <article><span><strong>{BUILTIN_ZH_EN_PACK.name}</strong><small>中文（简体） ⇄ English · {Object.keys(BUILTIN_ZH_EN_PACK.entries).length} 词条</small></span><em>内置</em></article>
          {packs.map((pack) => (
            <article key={pack.id}>
              <span><strong>{pack.name}</strong><small>{languageLabel(pack.source)} ⇄ {languageLabel(pack.target)} · {Object.keys(pack.entries).length} 词条</small></span>
              <button aria-label={`删除语言包 ${pack.name}`} onClick={() => savePacks(packs.filter((candidate) => candidate.id !== pack.id))} type="button"><Trash2 size={14} /></button>
            </article>
          ))}
          <button className="translation-workbench__import" onClick={() => packInputRef.current?.click()} type="button"><PackagePlus size={15} />导入本地 JSON 语言包</button>
          <input accept="application/json,.json" hidden onChange={(event) => {
            const file = event.target.files?.[0];
            if (file) void importPack(file);
          }} ref={packInputRef} type="file" />
        </aside>
      ) : null}
    </section>
  );
}
