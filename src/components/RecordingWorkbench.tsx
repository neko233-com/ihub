import {
  AppWindow,
  Check,
  Download,
  Gauge,
  MonitorUp,
  MousePointer2,
  Pause,
  Play,
  ScreenShare,
  Settings2,
  Square,
  Video,
  Volume2,
  X,
} from "lucide-react";
import type { RecordingQuality, RecordingSourcePreference } from "../lib/screen-recording";

export type RecordingPhase = "idle" | "starting" | "recording" | "paused" | "stopping";
export type { RecordingQuality, RecordingSourcePreference } from "../lib/screen-recording";

export interface RecordingResult {
  durationMs: number;
  mimeType: string;
  name: string;
  size: number;
  url: string;
}

interface RecordingWorkbenchProps {
  bytes: number;
  elapsedMs: number;
  includeSystemAudio: boolean;
  maximumBytes: number;
  maximumDurationMs: number;
  onClose: () => void;
  onIncludeSystemAudioChange: (value: boolean) => void;
  onPause: () => void;
  onQualityChange: (value: RecordingQuality) => void;
  onResume: () => void;
  onSourcePreferenceChange: (value: RecordingSourcePreference) => void;
  onStart: () => void;
  onStartWindowDrag?: () => void;
  onStop: () => void;
  phase: RecordingPhase;
  quality: RecordingQuality;
  result: RecordingResult | null;
  sourceName: string | null;
  sourcePreference: RecordingSourcePreference;
}

const sourceOptions: Array<{
  description: string;
  icon: typeof MonitorUp;
  id: RecordingSourcePreference;
  label: string;
}> = [
  { id: "monitor", label: "整个屏幕", description: "演示与完整工作流", icon: MonitorUp },
  { id: "window", label: "应用窗口", description: "只分享一个窗口", icon: AppWindow },
  { id: "browser", label: "浏览器标签", description: "适合网页与演示", icon: ScreenShare },
];

const qualityOptions: Array<{
  bitrate: string;
  fps: string;
  id: RecordingQuality;
  label: string;
}> = [
  { id: "compact", label: "轻量", fps: "24 FPS", bitrate: "3 Mbps" },
  { id: "balanced", label: "标准", fps: "30 FPS", bitrate: "6 Mbps" },
  { id: "smooth", label: "流畅", fps: "60 FPS", bitrate: "10 Mbps" },
];

function formatElapsed(milliseconds: number): string {
  const totalSeconds = Math.max(0, Math.floor(milliseconds / 1_000));
  const hours = Math.floor(totalSeconds / 3_600);
  const minutes = Math.floor((totalSeconds % 3_600) / 60);
  const seconds = totalSeconds % 60;
  return hours > 0
    ? [hours, minutes, seconds].map((value) => String(value).padStart(2, "0")).join(":")
    : [minutes, seconds].map((value) => String(value).padStart(2, "0")).join(":");
}

function formatBytes(bytes: number): string {
  if (bytes < 1_024) return `${bytes} B`;
  if (bytes < 1_024 * 1_024) return `${(bytes / 1_024).toFixed(1)} KiB`;
  return `${(bytes / (1_024 * 1_024)).toFixed(1)} MiB`;
}

function statusCopy(phase: RecordingPhase): { eyebrow: string; title: string } {
  if (phase === "starting") return { eyebrow: "WAITING FOR PERMISSION", title: "在系统选择器中确认录制目标" };
  if (phase === "recording") return { eyebrow: "RECORDING", title: "正在录制" };
  if (phase === "paused") return { eyebrow: "PAUSED", title: "录制已暂停" };
  if (phase === "stopping") return { eyebrow: "SAVING WEBM", title: "正在封装并保存视频" };
  return { eyebrow: "READY", title: "选择来源，开始录制" };
}

export function RecordingWorkbench({
  bytes,
  elapsedMs,
  includeSystemAudio,
  maximumBytes,
  maximumDurationMs,
  onClose,
  onIncludeSystemAudioChange,
  onPause,
  onQualityChange,
  onResume,
  onSourcePreferenceChange,
  onStart,
  onStartWindowDrag,
  onStop,
  phase,
  quality,
  result,
  sourceName,
  sourcePreference,
}: RecordingWorkbenchProps) {
  const active = phase !== "idle";
  const copy = statusCopy(phase);
  const selectedQuality = qualityOptions.find((option) => option.id === quality) ?? qualityOptions[1];

  const downloadAgain = () => {
    if (!result) return;
    const anchor = document.createElement("a");
    anchor.href = result.url;
    anchor.download = result.name;
    anchor.click();
  };

  return (
    <section aria-label="屏幕录制工作台" className={`recording-workbench is-${phase}`}>
      <header
        className="recording-workbench__header"
        data-tauri-drag-region="true"
        onMouseDown={(event) => {
          if (event.button === 0 && event.target === event.currentTarget) onStartWindowDrag?.();
        }}
      >
        <div className="recording-workbench__identity">
          <span><Video size={18} /></span>
          <div><strong id="recording-workbench-title">录屏助手</strong><small>Screen Recorder</small></div>
        </div>
        <div className="recording-workbench__local"><Check size={14} /><span>本机捕获 · WebM</span></div>
        <button aria-label="关闭录屏助手" onClick={onClose} type="button"><X size={17} /></button>
      </header>

      <main className="recording-workbench__body">
        <aside className="recording-workbench__sources">
          <div className="recording-workbench__section-title"><MousePointer2 size={14} /><span>录制来源偏好</span></div>
          <div aria-label="录制来源偏好" className="recording-workbench__source-list" role="radiogroup">
            {sourceOptions.map((option) => {
              const Icon = option.icon;
              return (
                <button
                  aria-checked={sourcePreference === option.id}
                  className={sourcePreference === option.id ? "is-selected" : ""}
                  disabled={active}
                  key={option.id}
                  onClick={() => onSourcePreferenceChange(option.id)}
                  role="radio"
                  type="button"
                >
                  <span><Icon size={18} /></span>
                  <div><strong>{option.label}</strong><small>{option.description}</small></div>
                  <i />
                </button>
              );
            })}
          </div>
          <p>这是传给系统选择器的首选类型。Windows 最终仍会让你确认实际共享目标。</p>
        </aside>

        <section className="recording-workbench__stage">
          <div className="recording-workbench__status">
            <span className="recording-workbench__status-dot" />
            <div><small>{copy.eyebrow}</small><strong>{copy.title}</strong></div>
          </div>

          <div className="recording-workbench__canvas">
            {result && phase === "idle" ? (
              <video aria-label="最近一次录屏预览" controls playsInline preload="metadata" src={result.url} />
            ) : (
              <div className="recording-workbench__canvas-placeholder">
                <div className="recording-workbench__capture-frame">
                  <div /><div /><div />
                  <span><Video size={30} /></span>
                </div>
                <strong>{active ? formatElapsed(elapsedMs) : "00:00"}</strong>
                <small>{sourceName ?? (phase === "starting" ? "等待系统授权" : "尚未选择录制来源")}</small>
              </div>
            )}
          </div>

          <div className="recording-workbench__meter" aria-label="录制容量">
            <div><span>{formatBytes(bytes)} / {formatBytes(maximumBytes)}</span><span>{formatElapsed(Math.max(0, maximumDurationMs - elapsedMs))} 可用</span></div>
            <progress max={maximumBytes} value={Math.min(bytes, maximumBytes)} />
          </div>

          <div className="recording-workbench__actions">
            {phase === "recording" || phase === "paused" ? (
              <button className="recording-workbench__pause" onClick={phase === "paused" ? onResume : onPause} type="button">
                {phase === "paused" ? <Play size={16} /> : <Pause size={16} />}
                {phase === "paused" ? "继续" : "暂停"}
              </button>
            ) : null}
            <button
              className={`recording-workbench__primary${active ? " is-stop" : ""}`}
              disabled={phase === "stopping"}
              onClick={active ? onStop : onStart}
              type="button"
            >
              {active ? <Square size={15} fill="currentColor" /> : <Video size={17} />}
              {phase === "starting" ? "取消等待" : phase === "stopping" ? "正在保存…" : active ? "停止并保存" : "打开系统选择器并录制"}
            </button>
          </div>

          {result && phase === "idle" ? (
            <div className="recording-workbench__result">
              <span><Check size={14} /><strong>最近录制已保存</strong><small>{formatElapsed(result.durationMs)} · {formatBytes(result.size)}</small></span>
              <button onClick={downloadAgain} type="button"><Download size={14} />再次下载</button>
            </div>
          ) : null}
        </section>

        <aside className="recording-workbench__settings">
          <div className="recording-workbench__section-title"><Settings2 size={14} /><span>录制设置</span></div>
          <label className="recording-workbench__audio">
            <span><Volume2 size={17} /><span><strong>系统音频</strong><small>可用性取决于目标与 Windows</small></span></span>
            <input
              checked={includeSystemAudio}
              disabled={active}
              onChange={(event) => onIncludeSystemAudioChange(event.target.checked)}
              role="switch"
              type="checkbox"
            />
          </label>

          <div className="recording-workbench__quality-heading"><Gauge size={15} /><span>画面质量</span></div>
          <div className="recording-workbench__quality-list">
            {qualityOptions.map((option) => (
              <button
                className={quality === option.id ? "is-selected" : ""}
                disabled={active}
                key={option.id}
                onClick={() => onQualityChange(option.id)}
                type="button"
              >
                <span><strong>{option.label}</strong><small>{option.fps}</small></span>
                <em>{option.bitrate}</em>
              </button>
            ))}
          </div>

          <div className="recording-workbench__facts">
            <div><span>当前配置</span><strong>{selectedQuality.fps} · {selectedQuality.bitrate}</strong></div>
            <div><span>文件格式</span><strong>WebM（VP9 / VP8）</strong></div>
            <div><span>自动保护</span><strong>30 分钟 · 512 MiB</strong></div>
          </div>
          <p>稳定 MP4 转码、全局录制快捷键、按键显示和鼠标点击高亮需要独立原生插件；这里不会记录键盘输入。</p>
        </aside>
      </main>

      <footer className="recording-workbench__footer">
        <span><Check size={13} />显示器 / 窗口 / 标签页</span>
        <span><Check size={13} />暂停与继续</span>
        <span><Check size={13} />来源结束自动保存</span>
        <span>关闭工作台会停止并保存</span>
      </footer>
    </section>
  );
}
