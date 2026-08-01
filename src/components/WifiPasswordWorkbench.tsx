import {
  CircleAlert,
  Copy,
  Eye,
  EyeOff,
  KeyRound,
  LoaderCircle,
  LockKeyhole,
  RefreshCw,
  Search,
  ShieldCheck,
  TimerReset,
  Wifi,
  X,
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { command, isDesktop } from "../lib/desktop";

interface WifiProfileView {
  id: string;
  name: string;
  interfaceName: string;
  authentication: string;
  encryption: string;
  canReveal: boolean;
  groupPolicy: boolean;
}

interface WifiPasswordReveal {
  profileId: string;
  profileName: string;
  password: string;
}

interface WifiPasswordWorkbenchProps {
  onClose: () => void;
  onCopy: (value: string, label: string) => Promise<void> | void;
  onStartWindowDrag?: () => void;
  onToast: (message: string) => void;
}

const browserProfiles: WifiProfileView[] = [
  { id: "browser-home", name: "Home Wi‑Fi", interfaceName: "Intel Wi‑Fi 6E", authentication: "WPA2PSK", encryption: "AES", canReveal: true, groupPolicy: false },
  { id: "browser-office", name: "Office 802.1X", interfaceName: "Intel Wi‑Fi 6E", authentication: "WPA2", encryption: "AES", canReveal: false, groupPolicy: true },
];

export function wifiSecurityLabel(profile: Pick<WifiProfileView, "authentication" | "encryption">): string {
  return [profile.authentication, profile.encryption].filter(Boolean).join(" · ");
}

export function WifiPasswordWorkbench({ onClose, onCopy, onStartWindowDrag, onToast }: WifiPasswordWorkbenchProps) {
  const desktop = isDesktop();
  const [profiles, setProfiles] = useState<WifiProfileView[]>(desktop ? [] : browserProfiles);
  const [selectedId, setSelectedId] = useState<string | null>(desktop ? null : browserProfiles[0].id);
  const [query, setQuery] = useState("");
  const [phase, setPhase] = useState<"loading" | "idle" | "revealing">(desktop ? "loading" : "idle");
  const [secret, setSecret] = useState<WifiPasswordReveal | null>(null);
  const [visible, setVisible] = useState(false);
  const [expiresIn, setExpiresIn] = useState(0);
  const [error, setError] = useState<string | null>(null);

  const clearSecret = () => {
    setSecret(null);
    setVisible(false);
    setExpiresIn(0);
  };

  const refresh = async () => {
    if (!desktop) return;
    clearSecret();
    setPhase("loading");
    setError(null);
    try {
      const next = await command<WifiProfileView[]>("list_wifi_profiles");
      setProfiles(next);
      setSelectedId((current) => next.some((profile) => profile.id === current) ? current : next[0]?.id ?? null);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setPhase("idle");
    }
  };

  useEffect(() => { void refresh(); return clearSecret; }, []);

  useEffect(() => {
    clearSecret();
  }, [selectedId]);

  useEffect(() => {
    if (!secret || expiresIn <= 0) return;
    const timer = window.setInterval(() => setExpiresIn((current) => Math.max(0, current - 1)), 1_000);
    return () => window.clearInterval(timer);
  }, [secret, expiresIn > 0]);

  useEffect(() => {
    if (secret && expiresIn === 0) clearSecret();
  }, [expiresIn, secret]);

  const filtered = useMemo(() => {
    const normalized = query.trim().toLocaleLowerCase();
    return profiles.filter((profile) => !normalized || `${profile.name} ${profile.interfaceName} ${profile.authentication} ${profile.encryption}`.toLocaleLowerCase().includes(normalized));
  }, [profiles, query]);
  const selected = profiles.find((profile) => profile.id === selectedId) ?? null;

  const reveal = async () => {
    if (!desktop || !selected?.canReveal || phase !== "idle") return;
    clearSecret();
    setPhase("revealing");
    setError(null);
    try {
      const result = await command<WifiPasswordReveal>("reveal_wifi_password", { profileId: selected.id });
      if (result.profileId !== selected.id || !result.password) throw new Error("Windows 返回的密码与所选配置不匹配。");
      setSecret(result);
      setVisible(true);
      setExpiresIn(60);
      onToast("Wi-Fi 密码仅在当前工作台显示 60 秒。关闭或切换配置会立即清除。");
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      onToast(message);
    } finally {
      setPhase("idle");
    }
  };

  const close = () => {
    clearSecret();
    onClose();
  };

  return (
    <section aria-label="Windows Wi-Fi 密码查看工作台" className="wifi-password-workbench">
      <header className="wifi-password-workbench__header" data-tauri-drag-region="true" onMouseDown={(event) => { if (event.button === 0 && event.target === event.currentTarget) onStartWindowDrag?.(); }}>
        <div className="wifi-password-workbench__identity"><span><Wifi size={18} /></span><div><strong id="wifi-password-workbench-title">Wi‑Fi 密码查看器</strong><small>Windows Native Wi‑Fi</small></div></div>
        <div className="wifi-password-workbench__privacy"><ShieldCheck size={14} /><span>单项授权 · 内存显示 · 60 秒清除</span></div>
        <button aria-label="关闭 Wi-Fi 密码查看器" onClick={close} type="button"><X size={17} /></button>
      </header>

      <main className="wifi-password-workbench__body">
        <aside className="wifi-password-workbench__profiles">
          <div className="wifi-password-workbench__search"><Search size={14} /><input aria-label="筛选 Wi-Fi 配置" onChange={(event) => setQuery(event.target.value)} placeholder="筛选 SSID / 安全类型" value={query} /><button aria-label="刷新 Wi-Fi 配置" disabled={!desktop || phase !== "idle"} onClick={() => void refresh()} type="button"><RefreshCw size={13} /></button></div>
          <div className="wifi-password-workbench__profile-count"><span>已保存配置</span><strong>{filtered.length}</strong></div>
          <div className="wifi-password-workbench__profile-list">
            {filtered.map((profile) => <button className={profile.id === selectedId ? "is-selected" : ""} key={profile.id} onClick={() => setSelectedId(profile.id)} type="button"><span><Wifi size={15} /></span><div><strong>{profile.name}</strong><small>{wifiSecurityLabel(profile)}</small></div>{profile.groupPolicy ? <i>策略</i> : null}</button>)}
            {!filtered.length ? <div className="wifi-password-workbench__empty"><Wifi size={27} /><span>{phase === "loading" ? "正在读取 Windows 配置…" : "没有匹配的 Wi-Fi 配置"}</span></div> : null}
          </div>
          <p>列表只读取配置名称与安全元数据，不包含密码。iHub 不扫描附近网络，也不连接或删除配置。</p>
        </aside>

        <section className="wifi-password-workbench__stage">
          {selected ? (
            <>
              <div className="wifi-password-workbench__network-icon"><Wifi size={42} /></div>
              <small>SAVED WLAN PROFILE</small>
              <h3>{selected.name}</h3>
              <p>{selected.interfaceName}</p>
              <div className="wifi-password-workbench__secret">
                <div><LockKeyhole size={15} /><span>{secret ? (visible ? secret.password : "•".repeat(Math.min(24, Math.max(8, secret.password.length)))) : "密码尚未读取"}</span>{secret ? <button aria-label={visible ? "隐藏 Wi-Fi 密码" : "显示 Wi-Fi 密码"} onClick={() => setVisible((current) => !current)} type="button">{visible ? <EyeOff size={15} /> : <Eye size={15} />}</button> : null}</div>
                {secret ? <div className="wifi-password-workbench__secret-actions"><button onClick={() => void onCopy(secret.password, `${selected.name} Wi-Fi 密码`)} type="button"><Copy size={14} />复制密码</button><button onClick={clearSecret} type="button"><X size={14} />立即清除</button><span><TimerReset size={13} />{expiresIn}s</span></div> : null}
              </div>
              <button className="wifi-password-workbench__reveal" disabled={!desktop || !selected.canReveal || phase !== "idle"} onClick={() => void reveal()} type="button">{phase === "revealing" ? <LoaderCircle className="spin" size={17} /> : <KeyRound size={17} />}{phase === "revealing" ? "等待 Windows UAC 与密钥…" : selected.canReveal ? "请求查看此配置密码" : "此配置没有预共享密钥"}</button>
              <p className="wifi-password-workbench__uac-note"><CircleAlert size={14} />微软默认只向本机管理员授予明文密钥权限，因此每次读取都会出现 UAC；取消即不读取。</p>
              {!desktop ? <p className="wifi-password-workbench__browser-note"><CircleAlert size={14} />浏览器预览不枚举真实 SSID、不触发 UAC，也不展示示例密码。</p> : null}
              {error ? <p className="wifi-password-workbench__error"><CircleAlert size={14} />{error}</p> : null}
            </>
          ) : <div className="wifi-password-workbench__stage-empty"><Wifi size={58} /><strong>选择一个已保存配置</strong><small>只有明确点击“请求查看”才会读取该配置的密钥。</small></div>}
        </section>

        <aside className="wifi-password-workbench__details">
          <div className="wifi-password-workbench__section-title"><ShieldCheck size={14} /><span>配置与边界</span></div>
          <div className="wifi-password-workbench__facts"><article><small>认证</small><strong>{selected?.authentication ?? "—"}</strong></article><article><small>加密</small><strong>{selected?.encryption ?? "—"}</strong></article><article><small>适配器</small><strong>{selected?.interfaceName ?? "—"}</strong></article><article><small>配置来源</small><strong>{selected?.groupPolicy ? "组策略" : "本机保存"}</strong></article></div>
          <div className="wifi-password-workbench__boundary"><LockKeyhole size={15} /><span><strong>凭据生命周期</strong><small>Native Wi‑Fi XML 的明文 UTF‑16 缓冲会在 Rust 中主动清零；管理员辅助程序通过随机本机命名管道返回，父进程核对实际客户端 PID。密码不写文件、不进日志、不交给插件。</small></span></div>
          <div className="wifi-password-workbench__boundary is-warning"><Copy size={15} /><span><strong>剪贴板是显式出口</strong><small>只有点击“复制密码”才写入系统剪贴板；复制后由你负责清除，iHub 不会覆盖后来复制的新内容。</small></span></div>
        </aside>
      </main>

      <footer className="wifi-password-workbench__footer"><span><ShieldCheck size={12} />不调用 netsh / PowerShell · 不修改 WLAN</span><span>Windows DACL 决定是否可读</span><span>{secret ? `密钥将在 ${expiresIn}s 后清除` : "当前内存无密钥"}</span></footer>
    </section>
  );
}
