import {
  Check,
  CircleAlert,
  Copy,
  FileImage,
  Image as ImageIcon,
  Languages,
  LoaderCircle,
  MonitorDown,
  RotateCcw,
  ScanText,
  ShieldCheck,
  Sparkles,
  X,
} from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { command, isDesktop } from "../lib/desktop";
import { prepareOcrPng } from "../lib/ocr-image";
import {
  createRegionCaptureDemoSource,
  validateRegionCaptureSize,
  type CroppedCapture,
  type RegionCaptureSource,
} from "../lib/region-capture";
import { RegionCaptureEditor } from "./RegionCaptureEditor";

interface NativeScreenshot {
  dataUrl: string;
  name: string;
  mimeType: "image/png";
  width: number;
  height: number;
  displayIndex: number;
}

interface OcrLanguageInfo {
  tag: string;
  displayName: string;
  nativeName: string;
}

interface OcrCapabilities {
  available: boolean;
  engine: string;
  maxImageDimension: number;
  maxPngBytes: number;
  maxTextBytes: number;
  languages: OcrLanguageInfo[];
}

interface OcrRecognitionResult {
  text: string;
  language: string;
  lineCount: number;
  width: number;
  height: number;
  truncated: boolean;
}

interface OcrWorkbenchProps {
  onClose: () => void;
  onCopy: (value: string, label: string) => Promise<void> | void;
  onStartWindowDrag?: () => void;
  onToast: (message: string) => void;
}

function revokeSource(source: RegionCaptureSource | null) {
  if (source?.revokeOnClose && source.url.startsWith("blob:")) URL.revokeObjectURL(source.url);
}

function imageSize(url: string): Promise<{ width: number; height: number }> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.decoding = "async";
    image.onload = () => resolve({ width: image.naturalWidth, height: image.naturalHeight });
    image.onerror = () => reject(new Error("无法读取所选图片。"));
    image.src = url;
  });
}

export function OcrWorkbench({ onClose, onCopy, onStartWindowDrag, onToast }: OcrWorkbenchProps) {
  const desktop = isDesktop();
  const sourceRef = useRef<RegionCaptureSource | null>(null);
  const resultPreviewRef = useRef<string | null>(null);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const [source, setSource] = useState<RegionCaptureSource | null>(() => desktop ? null : createRegionCaptureDemoSource());
  const [capabilities, setCapabilities] = useState<OcrCapabilities | null>(null);
  const [language, setLanguage] = useState("");
  const [result, setResult] = useState<OcrRecognitionResult | null>(null);
  const [resultPreview, setResultPreview] = useState<string | null>(null);
  const [phase, setPhase] = useState<"idle" | "capturing" | "recognizing">("idle");
  const [status, setStatus] = useState(desktop ? "选择屏幕区域或本地图片开始识别。" : "浏览器开发预览不会调用 Windows OCR。");
  const [error, setError] = useState<string | null>(null);

  const replaceSource = (next: RegionCaptureSource | null) => {
    revokeSource(sourceRef.current);
    sourceRef.current = next;
    setSource(next);
    setResult(null);
    if (resultPreviewRef.current) URL.revokeObjectURL(resultPreviewRef.current);
    resultPreviewRef.current = null;
    setResultPreview(null);
  };

  useEffect(() => {
    sourceRef.current = source;
    if (!desktop) return;
    let cancelled = false;
    void command<OcrCapabilities>("get_ocr_capabilities")
      .then((value) => {
        if (cancelled) return;
        setCapabilities(value);
        if (!value.available) setError("Windows 尚未安装可用的 OCR 语言包。");
      })
      .catch((cause) => {
        if (!cancelled) setError(cause instanceof Error ? cause.message : String(cause));
      });
    return () => {
      cancelled = true;
    };
  }, [desktop]);

  useEffect(() => () => {
    revokeSource(sourceRef.current);
    if (resultPreviewRef.current) URL.revokeObjectURL(resultPreviewRef.current);
  }, []);

  const captureScreen = async () => {
    if (!desktop || phase !== "idle") return;
    setPhase("capturing");
    setError(null);
    try {
      const screenshot = await command<NativeScreenshot>("capture_native_screenshot");
      if (screenshot.mimeType !== "image/png" || !screenshot.dataUrl.startsWith("data:image/png;base64,")) {
        throw new Error("宿主没有返回有效的 PNG 截图。");
      }
      validateRegionCaptureSize(screenshot);
      replaceSource({
        width: screenshot.width,
        height: screenshot.height,
        name: screenshot.name,
        url: screenshot.dataUrl,
      });
      setStatus("截图已进入内存。拖拽选区后点击“识别选区”。");
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      onToast(message);
    } finally {
      setPhase("idle");
    }
  };

  const chooseImage = async (file: File | undefined) => {
    if (!file) return;
    if (!/^image\/(?:png|jpeg)$/.test(file.type) || file.size > 16 * 1024 * 1024) {
      setError("请选择不超过 16 MiB 的 PNG 或 JPEG 图片。");
      return;
    }
    const url = URL.createObjectURL(file);
    try {
      const dimensions = validateRegionCaptureSize(await imageSize(url));
      replaceSource({ ...dimensions, name: file.name, url, revokeOnClose: true });
      setStatus("图片只在当前页面内存中读取。拖拽选区后开始本地识别。");
    } catch (cause) {
      URL.revokeObjectURL(url);
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      if (fileInputRef.current) fileInputRef.current.value = "";
    }
  };

  const recognize = async (capture: CroppedCapture) => {
    if (!desktop || !capabilities?.available) {
      setError("请在已安装 OCR 语言包的 Windows 桌面应用中执行识别。");
      return;
    }
    setPhase("recognizing");
    setError(null);
    try {
      const prepared = await prepareOcrPng(
        capture.blob,
        capture,
        capabilities.maxImageDimension,
        capabilities.maxPngBytes,
      );
      const nextResult = await command<OcrRecognitionResult>("recognize_ocr_image", {
        request: { dataUrl: prepared.dataUrl, language: language || null },
      });
      const preview = URL.createObjectURL(capture.blob);
      if (resultPreviewRef.current) URL.revokeObjectURL(resultPreviewRef.current);
      resultPreviewRef.current = preview;
      setResultPreview(preview);
      setResult(nextResult);
      setStatus(prepared.resized
        ? `选区已等比缩放到 ${prepared.size.width} × ${prepared.size.height} 后完成本地识别。`
        : "选区已在本机完成识别，图片与文字均未联网。",
      );
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      onToast(message);
    } finally {
      setPhase("idle");
    }
  };

  return (
    <section aria-label="屏幕 OCR 工作台" className="ocr-workbench">
      <header
        className="ocr-workbench__header"
        data-tauri-drag-region="true"
        onMouseDown={(event) => {
          if (event.button === 0 && event.target === event.currentTarget) onStartWindowDrag?.();
        }}
      >
        <div className="ocr-workbench__identity"><span><ScanText size={18} /></span><div><strong id="ocr-workbench-title">屏幕 OCR</strong><small>Screenshot to Text</small></div></div>
        <div className="ocr-workbench__privacy"><ShieldCheck size={14} /><span>Windows 本地识别 · 网络请求 0</span></div>
        <button aria-label="关闭屏幕 OCR" onClick={onClose} type="button"><X size={17} /></button>
      </header>

      <main className="ocr-workbench__body">
        <aside className="ocr-workbench__sources">
          <div className="ocr-workbench__section-title"><ImageIcon size={14} /><span>图像来源</span></div>
          <button disabled={!desktop || phase !== "idle"} onClick={() => void captureScreen()} type="button">
            <span><MonitorDown size={18} /></span><div><strong>截取主显示器</strong><small>隐藏 iHub 后读取一帧</small></div>
          </button>
          <button disabled={phase !== "idle"} onClick={() => fileInputRef.current?.click()} type="button">
            <span><FileImage size={18} /></span><div><strong>选择本地图片</strong><small>PNG / JPEG · 最大 16 MiB</small></div>
          </button>
          <input accept="image/png,image/jpeg" aria-label="选择 OCR 图片" hidden onChange={(event) => void chooseImage(event.target.files?.[0])} ref={fileInputRef} type="file" />
          <p>截图和选区只存在于本次页面内存；关闭工作台即释放。普通插件无法调用这条像素通道。</p>
          <div className="ocr-workbench__source-facts">
            <div><span>当前来源</span><strong>{source?.name ?? "未选择"}</strong></div>
            <div><span>原始尺寸</span><strong>{source ? `${source.width} × ${source.height}` : "—"}</strong></div>
            <div><span>识别引擎</span><strong>{capabilities?.engine ?? (desktop ? "正在检查" : "桌面端")}</strong></div>
          </div>
        </aside>

        <section className="ocr-workbench__stage">
          {phase === "capturing" ? <div className="ocr-workbench__empty"><LoaderCircle className="spin" size={31} /><strong>正在隐藏 iHub 并截取显示器…</strong><small>只读取一次，不会持续录屏。</small></div> : result && resultPreview ? (
            <div className="ocr-workbench__preview">
              <img alt="已识别 OCR 选区" src={resultPreview} />
              <div><Check size={15} /><span><strong>识别完成</strong><small>{result.width} × {result.height} · {result.lineCount} 行 · {result.language}</small></span></div>
              <button onClick={() => { setResult(null); setStatus("可重新拖拽选区识别。"); }} type="button"><RotateCcw size={14} />重新选区</button>
            </div>
          ) : source ? (
            <RegionCaptureEditor
              developmentPreview={!desktop}
              exportLabel={phase === "recognizing" ? "正在识别…" : "识别选区"}
              onCancel={() => replaceSource(desktop ? null : createRegionCaptureDemoSource())}
              onExport={recognize}
              onStatus={(message) => setError(message)}
              source={source}
            />
          ) : (
            <div className="ocr-workbench__empty"><ScanText size={42} /><strong>截取屏幕，框选文字区域</strong><small>建议只框住需要的文字，以获得更高准确率。</small><button disabled={!desktop} onClick={() => void captureScreen()} type="button"><MonitorDown size={16} />开始截图 OCR</button></div>
          )}
          {phase === "recognizing" ? <div aria-live="polite" className="ocr-workbench__busy"><LoaderCircle className="spin" size={18} />Windows 正在本地识别选区…</div> : null}
        </section>

        <aside className="ocr-workbench__result">
          <div className="ocr-workbench__section-title"><Languages size={14} /><span>语言与结果</span></div>
          <label><span>识别语言</span><select disabled={!capabilities?.available || phase !== "idle"} onChange={(event) => setLanguage(event.target.value)} value={language}><option value="">自动（用户首选语言）</option>{capabilities?.languages.map((item) => <option key={item.tag} value={item.tag}>{item.nativeName} · {item.tag}</option>)}</select></label>
          <div className="ocr-workbench__language-note"><Sparkles size={14} /><span>{capabilities ? `已安装 ${capabilities.languages.length} 个 OCR 语言包 · 单边上限 ${capabilities.maxImageDimension}px` : desktop ? "正在读取 Windows OCR 语言包…" : "桌面端会读取 Windows 已安装语言包"}</span></div>
          <div className="ocr-workbench__output-heading"><span>识别文字</span>{result?.text ? <button onClick={() => void onCopy(result.text, "OCR 文字")} type="button"><Copy size={13} />复制</button> : null}</div>
          <div aria-label="OCR 识别文字" className={`ocr-workbench__output${result?.text ? " has-text" : ""}`}>{result?.text || "识别结果会显示在这里。保留原始换行，可直接复制。"}</div>
          {result?.truncated ? <p className="ocr-workbench__warning"><CircleAlert size={13} />结果超过 256 KiB，已在字符边界截断。</p> : null}
          {error ? <p className="ocr-workbench__error"><CircleAlert size={13} />{error}</p> : null}
        </aside>
      </main>

      <footer className="ocr-workbench__footer"><span><ShieldCheck size={12} />{status}</span><span>内存 PNG · 无临时文件 · 无云端 OCR</span><span>{desktop ? "Windows 10/11 x64" : "开发预览"}</span></footer>
    </section>
  );
}
