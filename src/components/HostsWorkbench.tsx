import {
  Check,
  CircleAlert,
  FileLock2,
  ListChecks,
  LoaderCircle,
  Plus,
  RefreshCw,
  RotateCcw,
  Save,
  ServerCog,
  ShieldCheck,
  Trash2,
  X,
} from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { command, isDesktop } from "../lib/desktop";

interface HostsEntryView {
  id: string;
  ip: string;
  domains: string[];
  comment?: string;
  enabled: boolean;
  managed: boolean;
  lineNumber: number;
}

interface HostsSnapshot {
  fingerprint: string;
  entries: HostsEntryView[];
  managedEntries: HostsEntryView[];
  sizeBytes: number;
  lineEnding: string;
  canWriteDirectly: boolean;
  backupAvailable: boolean;
}

interface HostsApplyResult {
  snapshot: HostsSnapshot;
  elevated: boolean;
  backupCreated: boolean;
}

interface EditableHostsEntry {
  id: string;
  ip: string;
  domains: string;
  comment: string;
  enabled: boolean;
}

interface HostsWorkbenchProps {
  onClose: () => void;
  onStartWindowDrag?: () => void;
  onToast: (message: string) => void;
}

const browserSnapshot: HostsSnapshot = {
  fingerprint: "browser-preview-no-system-file",
  entries: [
    { id: "external-1", ip: "127.0.0.1", domains: ["localhost"], enabled: true, managed: false, lineNumber: 1 },
    { id: "managed-4", ip: "0.0.0.0", domains: ["telemetry.example.test"], comment: "示例规则", enabled: false, managed: true, lineNumber: 4 },
  ],
  managedEntries: [
    { id: "managed-4", ip: "0.0.0.0", domains: ["telemetry.example.test"], comment: "示例规则", enabled: false, managed: true, lineNumber: 4 },
  ],
  sizeBytes: 0,
  lineEnding: "CRLF",
  canWriteDirectly: false,
  backupAvailable: false,
};

function editableEntries(snapshot: HostsSnapshot): EditableHostsEntry[] {
  return snapshot.managedEntries.map((entry) => ({
    id: entry.id,
    ip: entry.ip,
    domains: entry.domains.join(" "),
    comment: entry.comment ?? "",
    enabled: entry.enabled,
  }));
}

function normalizedEntries(entries: EditableHostsEntry[]) {
  return entries.map((entry) => ({
    ip: entry.ip.trim(),
    domains: entry.domains.split(/[\s,]+/u).map((value) => value.trim()).filter(Boolean),
    comment: entry.comment.trim() || null,
    enabled: entry.enabled,
  }));
}

function fingerprintLabel(value: string): string {
  return value.length === 64 ? `${value.slice(0, 8)}…${value.slice(-6)}` : "浏览器预览";
}

export function HostsWorkbench({ onClose, onStartWindowDrag, onToast }: HostsWorkbenchProps) {
  const desktop = isDesktop();
  const [snapshot, setSnapshot] = useState<HostsSnapshot | null>(desktop ? null : browserSnapshot);
  const [entries, setEntries] = useState<EditableHostsEntry[]>(desktop ? [] : editableEntries(browserSnapshot));
  const [selectedId, setSelectedId] = useState<string | null>(desktop ? null : browserSnapshot.managedEntries[0]?.id ?? null);
  const [phase, setPhase] = useState<"loading" | "idle" | "applying" | "restoring">(desktop ? "loading" : "idle");
  const [confirming, setConfirming] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const refresh = async () => {
    if (!desktop) return;
    setPhase("loading");
    setError(null);
    try {
      const next = await command<HostsSnapshot>("get_hosts_snapshot");
      const nextEntries = editableEntries(next);
      setSnapshot(next);
      setEntries(nextEntries);
      setSelectedId((current) => nextEntries.some((entry) => entry.id === current) ? current : nextEntries[0]?.id ?? null);
      setDirty(false);
      setConfirming(false);
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setPhase("idle");
    }
  };

  useEffect(() => { void refresh(); }, []);

  const selected = entries.find((entry) => entry.id === selectedId) ?? null;
  const externalEntries = useMemo(() => snapshot?.entries.filter((entry) => !entry.managed) ?? [], [snapshot]);

  const updateSelected = (patch: Partial<EditableHostsEntry>) => {
    if (!selectedId) return;
    setEntries((current) => current.map((entry) => entry.id === selectedId ? { ...entry, ...patch } : entry));
    setDirty(true);
    setConfirming(false);
  };

  const addEntry = () => {
    const id = `new-${crypto.randomUUID()}`;
    setEntries((current) => [...current, { id, ip: "127.0.0.1", domains: "", comment: "", enabled: true }]);
    setSelectedId(id);
    setDirty(true);
    setConfirming(false);
  };

  const removeSelected = () => {
    if (!selectedId) return;
    setEntries((current) => current.filter((entry) => entry.id !== selectedId));
    setSelectedId(null);
    setDirty(true);
    setConfirming(false);
  };

  const apply = async () => {
    if (!desktop || !snapshot || phase !== "idle" || !dirty) return;
    if (!confirming) {
      setConfirming(true);
      return;
    }
    setPhase("applying");
    setError(null);
    try {
      const result = await command<HostsApplyResult>("apply_hosts_entries", {
        expectedFingerprint: snapshot.fingerprint,
        entries: normalizedEntries(entries),
      });
      setSnapshot(result.snapshot);
      const nextEntries = editableEntries(result.snapshot);
      setEntries(nextEntries);
      setSelectedId(nextEntries[0]?.id ?? null);
      setDirty(false);
      setConfirming(false);
      onToast(result.elevated ? "hosts 已经 UAC 授权并原子写入，旧文件已备份。" : "hosts 已原子写入，旧文件已备份。");
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      onToast(message);
    } finally {
      setPhase("idle");
    }
  };

  const restore = async () => {
    if (!desktop || !snapshot?.backupAvailable || phase !== "idle" || dirty) return;
    setPhase("restoring");
    setError(null);
    try {
      const result = await command<HostsApplyResult>("restore_hosts_backup", { expectedFingerprint: snapshot.fingerprint });
      setSnapshot(result.snapshot);
      const nextEntries = editableEntries(result.snapshot);
      setEntries(nextEntries);
      setSelectedId(nextEntries[0]?.id ?? null);
      onToast("已恢复上一份 hosts；写入前的文件现在成为可再次恢复的备份。");
    } catch (cause) {
      const message = cause instanceof Error ? cause.message : String(cause);
      setError(message);
      onToast(message);
    } finally {
      setPhase("idle");
    }
  };

  return (
    <section aria-label="Windows hosts 管理工作台" className="hosts-workbench">
      <header className="hosts-workbench__header" data-tauri-drag-region="true" onMouseDown={(event) => { if (event.button === 0 && event.target === event.currentTarget) onStartWindowDrag?.(); }}>
        <div className="hosts-workbench__identity"><span><ServerCog size={18} /></span><div><strong id="hosts-workbench-title">hosts 文件管理</strong><small>Windows Hosts Manager</small></div></div>
        <div className="hosts-workbench__privacy"><ShieldCheck size={14} /><span>固定系统路径 · 指纹校验 · 原子备份</span></div>
        <button aria-label="关闭 hosts 文件管理" onClick={onClose} type="button"><X size={17} /></button>
      </header>

      <main className="hosts-workbench__body">
        <aside className="hosts-workbench__managed">
          <div className="hosts-workbench__section-title"><ListChecks size={14} /><span>iHub 管理区</span><small>{entries.length}/256</small></div>
          <button className="hosts-workbench__add" disabled={!desktop || entries.length >= 256 || phase !== "idle"} onClick={addEntry} type="button"><Plus size={15} />新增映射</button>
          <div className="hosts-workbench__managed-list">
            {entries.map((entry) => (
              <button className={entry.id === selectedId ? "is-selected" : ""} key={entry.id} onClick={() => setSelectedId(entry.id)} type="button">
                <span className={entry.enabled ? "is-on" : ""} />
                <div><strong>{entry.domains.trim() || "未填写域名"}</strong><small>{entry.ip || "未填写 IP"}</small></div>
              </button>
            ))}
            {!entries.length ? <div className="hosts-workbench__empty"><FileLock2 size={25} /><span>还没有 iHub 管理的映射</span></div> : null}
          </div>
          <p>只编辑带 iHub 标记的区块；系统与其他软件维护的行保持原始字节不变。</p>
        </aside>

        <section className="hosts-workbench__editor">
          <div className="hosts-workbench__editor-heading"><div><small>MAPPING EDITOR</small><strong>{selected ? "编辑映射" : "选择或新增一条映射"}</strong></div>{selected ? <button aria-label="删除所选 hosts 映射" disabled={!desktop || phase !== "idle"} onClick={removeSelected} type="button"><Trash2 size={15} /></button> : null}</div>
          {selected ? (
            <div className="hosts-workbench__form">
              <label><span>IP 地址</span><input disabled={!desktop || phase !== "idle"} onChange={(event) => updateSelected({ ip: event.target.value })} placeholder="127.0.0.1 或 ::1" spellCheck={false} value={selected.ip} /></label>
              <label><span>域名（空格或逗号分隔，最多 8 个）</span><textarea disabled={!desktop || phase !== "idle"} onChange={(event) => updateSelected({ domains: event.target.value })} placeholder="example.test api.example.test" spellCheck={false} value={selected.domains} /></label>
              <label><span>备注（可选）</span><input disabled={!desktop || phase !== "idle"} maxLength={160} onChange={(event) => updateSelected({ comment: event.target.value })} placeholder="用途说明" value={selected.comment} /></label>
              <label className="hosts-workbench__toggle"><input checked={selected.enabled} disabled={!desktop || phase !== "idle"} onChange={(event) => updateSelected({ enabled: event.target.checked })} type="checkbox" /><span><strong>{selected.enabled ? "映射已启用" : "映射已暂停"}</strong><small>暂停时保留为 iHub 可识别的注释行</small></span></label>
            </div>
          ) : <div className="hosts-workbench__editor-empty"><ServerCog size={54} /><strong>受控编辑，不覆盖整份文件</strong><small>新增一条映射后再填写 IP、域名和备注。</small></div>}
          <div className={`hosts-workbench__apply${confirming ? " is-confirming" : ""}`}>
            {confirming ? <p><CircleAlert size={14} />将用当前指纹再次校验文件，创建备份后替换 iHub 管理区。普通权限会显示 Windows UAC。</p> : null}
            <button disabled={!desktop || !dirty || phase !== "idle"} onClick={() => void apply()} type="button">{phase === "applying" ? <LoaderCircle className="spin" size={16} /> : confirming ? <Check size={16} /> : <Save size={16} />}{phase === "applying" ? "正在等待系统写入…" : confirming ? "确认并请求系统写入" : "预览并应用更改"}</button>
            {dirty ? <button className="hosts-workbench__discard" disabled={phase !== "idle"} onClick={() => { if (snapshot) { const next = editableEntries(snapshot); setEntries(next); setSelectedId(next[0]?.id ?? null); setDirty(false); setConfirming(false); } }} type="button">放弃未应用更改</button> : null}
          </div>
          {!desktop ? <p className="hosts-workbench__browser-note"><CircleAlert size={14} />浏览器预览不读取或写入 Windows hosts，也不会触发 UAC。</p> : null}
          {error ? <p className="hosts-workbench__error"><CircleAlert size={14} />{error}</p> : null}
        </section>

        <aside className="hosts-workbench__system">
          <div className="hosts-workbench__section-title"><FileLock2 size={14} /><span>现有系统映射</span><button aria-label="刷新 hosts 文件" disabled={!desktop || phase !== "idle" || dirty} onClick={() => void refresh()} type="button"><RefreshCw size={13} /></button></div>
          <div className="hosts-workbench__facts"><article><small>文件</small><strong>{snapshot ? `${snapshot.sizeBytes} B` : "读取中"}</strong></article><article><small>换行</small><strong>{snapshot?.lineEnding ?? "—"}</strong></article><article><small>指纹</small><strong>{snapshot ? fingerprintLabel(snapshot.fingerprint) : "—"}</strong></article></div>
          <div className="hosts-workbench__external-list">
            {externalEntries.slice(0, 80).map((entry) => <article key={entry.id}><div><strong>{entry.domains.join(" ")}</strong><small>第 {entry.lineNumber} 行 · {entry.ip}</small></div><span>只读</span></article>)}
            {!externalEntries.length ? <div className="hosts-workbench__empty"><FileLock2 size={24} /><span>没有可显示的外部映射</span></div> : null}
          </div>
          <button className="hosts-workbench__restore" disabled={!desktop || !snapshot?.backupAvailable || dirty || phase !== "idle"} onClick={() => void restore()} type="button">{phase === "restoring" ? <LoaderCircle className="spin" size={15} /> : <RotateCcw size={15} />}{phase === "restoring" ? "正在恢复…" : "恢复上一份 iHub 备份"}</button>
          <p>恢复同样校验当前指纹并原子替换；有未应用编辑时不可恢复。</p>
        </aside>
      </main>

      <footer className="hosts-workbench__footer"><span><ShieldCheck size={12} />不向插件开放 · 不接受任意文件路径</span><span>{snapshot?.canWriteDirectly ? "当前进程已有写入权限" : "保存时按需请求一次 UAC"}</span><span>{dirty ? "有未应用更改" : "与磁盘快照一致"}</span></footer>
    </section>
  );
}

export { editableEntries as hostsEditableEntries, fingerprintLabel as hostsFingerprintLabel, normalizedEntries as normalizeHostsEntries };
