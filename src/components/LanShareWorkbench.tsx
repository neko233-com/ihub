import {
  Check,
  CircleAlert,
  Clock3,
  Copy,
  Download,
  Files,
  FolderUp,
  LoaderCircle,
  LockKeyhole,
  QrCode,
  Radio,
  Share2,
  ShieldCheck,
  Square,
  Wifi,
  X,
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { command, isDesktop } from "../lib/desktop";

interface LanSharedFileView {
  name: string;
  size: number;
}

interface LanFileShareView {
  url: string;
  files: LanSharedFileView[];
  totalBytes: number;
  downloadCount: number;
  bytesSent: number;
  startedAtEpochMs: number;
  expiresAtEpochMs: number;
  expiresInSeconds: number;
}

interface LanShareWorkbenchProps {
  onClose: () => void;
  onCopy: (value: string, label: string) => Promise<void> | void;
  onStartWindowDrag?: () => void;
  onToast: (message: string) => void;
}

function formatBytes(bytes: number): string {
  if (bytes < 1_024) return `${bytes} B`;
  if (bytes < 1_024 * 1_024) return `${(bytes / 1_024).toFixed(1)} KiB`;
  if (bytes < 1_024 * 1_024 * 1_024) return `${(bytes / (1_024 * 1_024)).toFixed(1)} MiB`;
  return `${(bytes / (1_024 * 1_024 * 1_024)).toFixed(1)} GiB`;
}

function remainingCopy(seconds: number): string {
  const safe = Math.max(0, Math.floor(seconds));
  const minutes = Math.floor(safe / 60);
  const rest = safe % 60;
  return `${String(minutes).padStart(2, "0")}:${String(rest).padStart(2, "0")}`;
}

export function LanShareWorkbench({ onClose, onCopy, onStartWindowDrag, onToast }: LanShareWorkbenchProps) {
  const desktop = isDesktop();
  const [share, setShare] = useState<LanFileShareView | null>(null);
  const [qrDataUrl, setQrDataUrl] = useState<string | null>(null);
  const [phase, setPhase] = useState<"idle" | "starting" | "stopping">("idle");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!desktop) return;
    let cancelled = false;
    const refresh = () => {
      void command<LanFileShareView | null>("get_lan_file_share_status")
        .then((value) => {
          if (!cancelled) setShare(value);
        })
        .catch(() => undefined);
    };
    refresh();
    const timer = window.setInterval(refresh, 1_000);
    return () => {
      cancelled = true;
      window.clearInterval(timer);
    };
  }, [desktop]);

  useEffect(() => {
    let cancelled = false;
    if (!share?.url) {
      setQrDataUrl(null);
      return;
    }
    void import("qrcode")
      .then(({ toDataURL }) => toDataURL(share.url, {
        errorCorrectionLevel: "M",
        margin: 2,
        width: 360,
        color: { dark: "#0d245fff", light: "#ffffffff" },
      }))
      .then((url) => {
        if (!cancelled) setQrDataUrl(url);
      })
      .catch(() => {
        if (!cancelled) setQrDataUrl(null);
      });
    return () => {
      cancelled = true;
    };
  }, [share?.url]);

  const progress = useMemo(
    () => share ? Math.max(0, Math.min(1, share.expiresInSeconds / (30 * 60))) : 0,
    [share],
  );

  const start = async () => {
    if (!desktop || phase !== "idle" || share) return;
    setPhase("starting");
    setError(null);
    try {
      const next = await command<LanFileShareView | null>("start_lan_file_share");
      if (next) {
        setShare(next);
        onToast("内网文件分享已启动，30 分钟后自动停止。");
      }
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      onToast(message);
    } finally {
      setPhase("idle");
    }
  };

  const stop = async () => {
    if (!desktop || phase !== "idle" || !share) return;
    setPhase("stopping");
    setError(null);
    try {
      await command<void>("stop_lan_file_share");
      setShare(null);
      setQrDataUrl(null);
      onToast("内网文件分享已停止。");
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      onToast(message);
    } finally {
      setPhase("idle");
    }
  };

  return (
    <section aria-label="内网文件分享工作台" className="lan-share-workbench">
      <header
        className="lan-share-workbench__header"
        data-tauri-drag-region="true"
        onMouseDown={(event) => {
          if (event.button === 0 && event.target === event.currentTarget) onStartWindowDrag?.();
        }}
      >
        <div className="lan-share-workbench__identity"><span><Share2 size={18} /></span><div><strong id="lan-share-workbench-title">内网文件分享</strong><small>LAN File Share</small></div></div>
        <div className="lan-share-workbench__privacy"><ShieldCheck size={14} /><span>随机链接 · 仅局域网 · 无广告</span></div>
        <button aria-label="关闭内网文件分享" onClick={onClose} type="button"><X size={17} /></button>
      </header>

      <main className="lan-share-workbench__body">
        <aside className="lan-share-workbench__setup">
          <div className="lan-share-workbench__section-title"><FolderUp size={14} /><span>分享来源</span></div>
          <button disabled={!desktop || phase !== "idle" || Boolean(share)} onClick={() => void start()} type="button">
            {phase === "starting" ? <LoaderCircle className="spin" size={19} /> : <Files size={19} />}
            <span><strong>{phase === "starting" ? "等待系统选择器…" : "选择并分享文件"}</strong><small>最多 32 个 · 总计 64 GiB</small></span>
          </button>
          <div className="lan-share-workbench__steps">
            <article><i>1</i><span><strong>选择文件</strong><small>宿主立即打开文件，页面不接收路径</small></span></article>
            <article><i>2</i><span><strong>同一局域网</strong><small>接收设备连接同一 Wi‑Fi / LAN</small></span></article>
            <article><i>3</i><span><strong>扫码下载</strong><small>随机 URL 在 30 分钟后失效</small></span></article>
          </div>
          <p>首次监听局域网时 Windows 防火墙可能询问权限；只应允许“专用网络”。公网、VPN 路由和端口转发不属于 iHub 的安全承诺。</p>
        </aside>

        <section className={`lan-share-workbench__stage${share ? " is-active" : ""}`}>
          <div className="lan-share-workbench__status"><span className={share ? "is-live" : ""} /><div><small>{share ? "SHARING ON LAN" : "READY"}</small><strong>{share ? "文件可供局域网设备下载" : "选择文件后生成扫码链接"}</strong></div></div>
          <div className="lan-share-workbench__qr">
            {qrDataUrl ? <img alt="内网文件分享二维码" src={qrDataUrl} /> : <div><QrCode size={58} /><small>{desktop ? "等待分享链接" : "桌面端生成二维码"}</small></div>}
          </div>
          <div className="lan-share-workbench__url"><Radio size={15} /><span>{share?.url ?? "http://本机内网地址/随机令牌/"}</span>{share ? <button aria-label="复制内网分享链接" onClick={() => void onCopy(share.url, "内网分享链接")} type="button"><Copy size={14} /></button> : null}</div>
          {share ? (
            <>
              <div className="lan-share-workbench__timer"><div><Clock3 size={14} /><span>剩余 {remainingCopy(share.expiresInSeconds)}</span><small>30 分钟自动停止</small></div><progress max={1} value={progress} /></div>
              <button className="lan-share-workbench__stop" disabled={phase !== "idle"} onClick={() => void stop()} type="button">{phase === "stopping" ? <LoaderCircle className="spin" size={16} /> : <Square fill="currentColor" size={14} />}{phase === "stopping" ? "正在停止…" : "立即停止分享"}</button>
            </>
          ) : <button className="lan-share-workbench__start" disabled={!desktop || phase !== "idle"} onClick={() => void start()} type="button"><Share2 size={17} />选择文件并开始分享</button>}
          {!desktop ? <p className="lan-share-workbench__desktop-note"><CircleAlert size={14} />浏览器预览不会打开端口或读取本机文件。</p> : null}
          {error ? <p className="lan-share-workbench__error"><CircleAlert size={14} />{error}</p> : null}
        </section>

        <aside className="lan-share-workbench__files">
          <div className="lan-share-workbench__section-title"><Download size={14} /><span>文件与传输</span></div>
          <div className="lan-share-workbench__stats"><article><small>文件</small><strong>{share?.files.length ?? 0}</strong></article><article><small>已下载</small><strong>{share?.downloadCount ?? 0}</strong></article><article><small>已发送</small><strong>{formatBytes(share?.bytesSent ?? 0)}</strong></article></div>
          <div className="lan-share-workbench__file-list">
            {share?.files.length ? share.files.map((file, index) => <article key={`${index}:${file.name}`}><span><Files size={15} /></span><div><strong>{file.name}</strong><small>{formatBytes(file.size)}</small></div><Check size={13} /></article>) : <div className="lan-share-workbench__empty-files"><Files size={28} /><span>尚未选择文件</span></div>}
          </div>
          <div className="lan-share-workbench__boundary"><LockKeyhole size={15} /><span><strong>下载专用</strong><small>不接受上传、目录路径、任意 URL、跨网段公网客户端或后台常驻分享。</small></span></div>
        </aside>
      </main>

      <footer className="lan-share-workbench__footer"><span><Wifi size={12} />仅接受私有、回环或链路本地来源地址</span><span>HTTP 局域网传输 · 随机 128-bit 路径</span><span>{share ? `${formatBytes(share.totalBytes)} 待分享` : "未监听端口"}</span></footer>
    </section>
  );
}

export { formatBytes as formatLanShareBytes, remainingCopy as lanShareRemainingCopy };
