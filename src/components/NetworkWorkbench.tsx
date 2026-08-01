import {
  Activity,
  Check,
  CircleAlert,
  Cloud,
  Copy,
  Gauge,
  Globe2,
  LoaderCircle,
  LockKeyhole,
  Network,
  RefreshCw,
  Router,
  ShieldCheck,
  Wifi,
  X,
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { command, isDesktop } from "../lib/desktop";

export interface LocalNetworkInfo {
  preferredIpv4: string | null;
  preferredIpv6: string | null;
  onlineRouteAvailable: boolean;
}

export interface PublicNetworkInfo {
  publicIp: string;
  addressFamily: string;
  edgeLocation: string | null;
  tlsVersion: string | null;
  httpProtocol: string | null;
  provider: string;
}

export interface NetworkSpeedResult {
  latencyMs: number;
  jitterMs: number;
  downloadMbps: number;
  uploadMbps: number;
  downloadBytes: number;
  uploadBytes: number;
  durationMs: number;
  provider: string;
}

interface NetworkWorkbenchProps {
  onClose: () => void;
  onCopy: (value: string, label: string) => Promise<void> | void;
  onStartWindowDrag?: () => void;
  onToast: (message: string) => void;
}

type NetworkPhase = "idle" | "locating" | "testing";

function formatMetric(value: number | null, unit: string): string {
  return value === null ? "—" : `${value.toFixed(1)} ${unit}`;
}

function speedGrade(result: NetworkSpeedResult | null): { label: string; detail: string } {
  if (!result) return { label: "等待测试", detail: "点击开始后测量空载延迟、抖动、下载与上传。" };
  if (result.latencyMs <= 35 && result.downloadMbps >= 50 && result.uploadMbps >= 10) {
    return { label: "连接优秀", detail: "适合高清视频、云端协作与实时通话。" };
  }
  if (result.latencyMs <= 80 && result.downloadMbps >= 15 && result.uploadMbps >= 3) {
    return { label: "连接良好", detail: "日常浏览、视频和远程办公通常顺畅。" };
  }
  return { label: "建议排查", detail: "可检查 Wi‑Fi 信号、代理、VPN 或运营商链路。" };
}

export function NetworkWorkbench({ onClose, onCopy, onStartWindowDrag, onToast }: NetworkWorkbenchProps) {
  const desktop = isDesktop();
  const [localInfo, setLocalInfo] = useState<LocalNetworkInfo | null>(null);
  const [publicInfo, setPublicInfo] = useState<PublicNetworkInfo | null>(null);
  const [result, setResult] = useState<NetworkSpeedResult | null>(null);
  const [phase, setPhase] = useState<NetworkPhase>("idle");
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!desktop) return;
    let cancelled = false;
    void command<LocalNetworkInfo>("get_local_network_info")
      .then((value) => {
        if (!cancelled) setLocalInfo(value);
      })
      .catch(() => {
        if (!cancelled) setLocalInfo({ preferredIpv4: null, preferredIpv6: null, onlineRouteAvailable: false });
      });
    return () => {
      cancelled = true;
    };
  }, [desktop]);

  const grade = useMemo(() => speedGrade(result), [result]);

  const locatePublicIp = async () => {
    if (!desktop || phase !== "idle") return;
    setPhase("locating");
    setError(null);
    try {
      setPublicInfo(await command<PublicNetworkInfo>("get_public_network_info"));
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      onToast(message);
    } finally {
      setPhase("idle");
    }
  };

  const startSpeedTest = async () => {
    if (!desktop || phase !== "idle") return;
    setPhase("testing");
    setError(null);
    setResult(null);
    try {
      setResult(await command<NetworkSpeedResult>("run_network_speed_test"));
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      onToast(message);
    } finally {
      setPhase("idle");
    }
  };

  return (
    <section aria-label="网络诊断工作台" className="network-workbench">
      <header
        className="network-workbench__header"
        data-tauri-drag-region="true"
        onMouseDown={(event) => {
          if (event.button === 0 && event.target === event.currentTarget) onStartWindowDrag?.();
        }}
      >
        <div className="network-workbench__identity">
          <span><Gauge size={18} /></span>
          <div><strong id="network-workbench-title">网络诊断</strong><small>IP &amp; Speed Test</small></div>
        </div>
        <div className="network-workbench__privacy"><ShieldCheck size={14} /><span>固定端点 · 用户触发</span></div>
        <button aria-label="关闭网络诊断" onClick={onClose} type="button"><X size={17} /></button>
      </header>

      <main className="network-workbench__body">
        <aside className="network-workbench__addresses">
          <div className="network-workbench__section-title"><Network size={14} /><span>地址与路由</span></div>
          <article>
            <span><Router size={17} /></span>
            <div><small>首选本地 IPv4</small><strong>{localInfo?.preferredIpv4 ?? "—"}</strong></div>
            {localInfo?.preferredIpv4 ? <button aria-label="复制本地 IPv4" onClick={() => void onCopy(localInfo.preferredIpv4!, "本地 IPv4")} type="button"><Copy size={13} /></button> : null}
          </article>
          <article>
            <span><Wifi size={17} /></span>
            <div><small>首选本地 IPv6</small><strong>{localInfo?.preferredIpv6 ?? "未发现"}</strong></div>
          </article>
          <article className="network-workbench__public-card">
            <span><Globe2 size={17} /></span>
            <div><small>公网 IP</small><strong>{publicInfo?.publicIp ?? "点击后查询"}</strong></div>
            {publicInfo?.publicIp ? <button aria-label="复制公网 IP" onClick={() => void onCopy(publicInfo.publicIp, "公网 IP")} type="button"><Copy size={13} /></button> : null}
          </article>
          <button className="network-workbench__lookup" disabled={!desktop || phase !== "idle"} onClick={() => void locatePublicIp()} type="button">
            {phase === "locating" ? <LoaderCircle className="spin" size={15} /> : <RefreshCw size={15} />}
            {phase === "locating" ? "正在查询…" : publicInfo ? "重新查询公网 IP" : "查询公网 IP"}
          </button>
          <p>公网查询会把你的 IP 暴露给 Cloudflare；iHub 不会保存或转发查询结果。</p>
        </aside>

        <section className="network-workbench__stage">
          <div className={`network-workbench__orb${phase === "testing" ? " is-testing" : ""}`}>
            <span><Activity size={25} /></span>
            <strong>{result ? result.downloadMbps.toFixed(1) : phase === "testing" ? "…" : "0.0"}</strong>
            <small>Mbps 下载</small>
          </div>
          <div className="network-workbench__grade">
            {result ? <Check size={15} /> : <Gauge size={15} />}
            <div><strong>{phase === "testing" ? "正在测量连接" : grade.label}</strong><small>{phase === "testing" ? "依次执行 6 次延迟、10 MB 下载和 5 MB 上传。" : grade.detail}</small></div>
          </div>
          <div className="network-workbench__metrics">
            <article><small>延迟</small><strong>{formatMetric(result?.latencyMs ?? null, "ms")}</strong><span>空载中位数</span></article>
            <article><small>抖动</small><strong>{formatMetric(result?.jitterMs ?? null, "ms")}</strong><span>相邻样本波动</span></article>
            <article><small>下载</small><strong>{formatMetric(result?.downloadMbps ?? null, "Mbps")}</strong><span>10 MB 固定载荷</span></article>
            <article><small>上传</small><strong>{formatMetric(result?.uploadMbps ?? null, "Mbps")}</strong><span>5 MB 零内容载荷</span></article>
          </div>
          <button className="network-workbench__start" disabled={!desktop || phase !== "idle"} onClick={() => void startSpeedTest()} type="button">
            {phase === "testing" ? <LoaderCircle className="spin" size={17} /> : <Gauge size={17} />}
            {phase === "testing" ? "测速进行中…" : result ? "重新测速" : "开始精简测速"}
          </button>
          {!desktop ? <p className="network-workbench__desktop-note"><CircleAlert size={14} />网络诊断只在 Windows 桌面应用中执行，浏览器预览不会联网。</p> : null}
          {error ? <p className="network-workbench__error"><CircleAlert size={14} />{error}</p> : null}
        </section>

        <aside className="network-workbench__facts">
          <div className="network-workbench__section-title"><LockKeyhole size={14} /><span>测试边界</span></div>
          <dl>
            <div><dt>服务端</dt><dd>Cloudflare Edge</dd></div>
            <div><dt>下载数据</dt><dd>10.0 MB</dd></div>
            <div><dt>上传数据</dt><dd>5.0 MB</dd></div>
            <div><dt>上传内容</dt><dd>固定字节，无本地文件</dd></div>
            <div><dt>任意 URL</dt><dd>不允许</dd></div>
            <div><dt>后台运行</dt><dd>不允许</dd></div>
          </dl>
          {publicInfo ? (
            <div className="network-workbench__edge">
              <Cloud size={17} />
              <div><small>边缘连接</small><strong>{[publicInfo.edgeLocation, publicInfo.addressFamily, publicInfo.tlsVersion, publicInfo.httpProtocol].filter(Boolean).join(" · ")}</strong></div>
            </div>
          ) : null}
          {result ? <p>本次用时 {(result.durationMs / 1_000).toFixed(1)} 秒。结果反映当前到 Cloudflare 任播边缘的链路，不代表所有网站或运营商承诺值。</p> : <p>测速会消耗约 15 MB 流量；移动热点或计费网络请谨慎使用。</p>}
        </aside>
      </main>

      <footer className="network-workbench__footer">
        <span><LockKeyhole size={12} />结果仅留在本次页面</span>
        <span>HTTPS · 固定域名 · 严格超时</span>
        <span>{localInfo?.onlineRouteAvailable ? "本机路由可用" : desktop ? "未发现可用路由" : "桌面端执行"}</span>
      </footer>
    </section>
  );
}

export { speedGrade };
