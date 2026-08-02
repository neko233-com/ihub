import {
  Bot,
  Check,
  KeyRound,
  LoaderCircle,
  Pencil,
  Plus,
  Server,
  Trash2,
  X,
} from "lucide-react";
import { useCallback, useEffect, useMemo, useState } from "react";
import { command, isDesktop } from "../lib/desktop";
import type {
  AiProviderModel,
  AiProviderProfile,
  AiProviderTestResult,
} from "../lib/types";

interface ProviderDraft {
  id?: string;
  label: string;
  endpoint: string;
  modelsText: string;
  defaultModel: string;
  apiKey: string;
  hasApiKey: boolean;
  removeApiKey: boolean;
  makeDefault: boolean;
}

const emptyDraft = (): ProviderDraft => ({
  label: "本地 Ollama",
  endpoint: "http://127.0.0.1:11434/v1",
  modelsText: "qwen3:8b",
  defaultModel: "qwen3:8b",
  apiKey: "",
  hasApiKey: false,
  removeApiKey: false,
  makeDefault: true,
});

function modelIds(text: string): string[] {
  return [...new Set(text
    .split(/[\n,]/)
    .map((value) => value.trim())
    .filter(Boolean))];
}

function profileDraft(profile: AiProviderProfile): ProviderDraft {
  return {
    id: profile.id,
    label: profile.label,
    endpoint: profile.endpoint.replace(/\/$/, ""),
    modelsText: profile.models.map((model) => model.id).join("\n"),
    defaultModel: profile.defaultModel,
    apiKey: "",
    hasApiKey: profile.hasApiKey,
    removeApiKey: false,
    makeDefault: profile.isDefault,
  };
}

export function AiProviderSettings() {
  const [profiles, setProfiles] = useState<AiProviderProfile[]>([]);
  const [draft, setDraft] = useState<ProviderDraft | null>(null);
  const [loading, setLoading] = useState(isDesktop());
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [confirmDeleteId, setConfirmDeleteId] = useState<string | null>(null);
  const [testingId, setTestingId] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    if (!isDesktop()) {
      setProfiles([]);
      setLoading(false);
      return;
    }
    setLoading(true);
    try {
      setProfiles(await command<AiProviderProfile[]>("list_ai_provider_profiles"));
      setError(null);
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : "无法读取 AI Provider。 ");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const draftModelIds = useMemo(
    () => modelIds(draft?.modelsText ?? ""),
    [draft?.modelsText],
  );

  useEffect(() => {
    if (!draft || draftModelIds.length === 0 || draftModelIds.includes(draft.defaultModel)) {
      return;
    }
    setDraft((current) => current ? { ...current, defaultModel: draftModelIds[0] } : null);
  }, [draft, draftModelIds]);

  const save = async () => {
    if (!draft || !isDesktop()) {
      setError("请在 iHub 桌面端配置 AI Provider。");
      return;
    }
    const ids = modelIds(draft.modelsText);
    if (!draft.label.trim() || !draft.endpoint.trim() || ids.length === 0) {
      setError("请填写 Provider 名称、/v1 端点和至少一个模型 ID。");
      return;
    }
    setSaving(true);
    setError(null);
    setNotice(null);
    const models: AiProviderModel[] = ids.map((id) => ({
      id,
      label: id,
      description: `${draft.label.trim()} 的 ${id}`,
    }));
    try {
      await command<AiProviderProfile>("save_ai_provider_profile", {
        input: {
          id: draft.id,
          label: draft.label.trim(),
          endpoint: draft.endpoint.trim(),
          models,
          defaultModel: ids.includes(draft.defaultModel) ? draft.defaultModel : ids[0],
          apiKey: draft.removeApiKey
            ? ""
            : draft.apiKey.trim()
              ? draft.apiKey.trim()
              : undefined,
          makeDefault: draft.makeDefault,
        },
      });
      setDraft(null);
      setNotice("AI Provider 已保存；API Key 仅保存在 iHub 加密存储中。");
      await refresh();
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : "无法保存 AI Provider。");
    } finally {
      setSaving(false);
    }
  };

  const remove = async (profile: AiProviderProfile) => {
    if (confirmDeleteId !== profile.id) {
      setConfirmDeleteId(profile.id);
      return;
    }
    setSaving(true);
    setError(null);
    try {
      await command<boolean>("delete_ai_provider_profile", { profileId: profile.id });
      setConfirmDeleteId(null);
      if (draft?.id === profile.id) setDraft(null);
      setNotice(`已删除 ${profile.label} 及其加密 API Key。`);
      await refresh();
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : "无法删除 AI Provider。");
    } finally {
      setSaving(false);
    }
  };

  const testProvider = async (profile: AiProviderProfile) => {
    setTestingId(profile.id);
    setError(null);
    setNotice(null);
    try {
      const result = await command<AiProviderTestResult>("test_ai_provider_profile", {
        profileId: profile.id,
      });
      const configured = new Set(profile.models.map((model) => model.id));
      const unconfigured = result.modelIds.filter((id) => !configured.has(id));
      setNotice(unconfigured.length > 0
        ? `${result.message} 另发现 ${unconfigured.length} 个尚未配置的模型。`
        : result.message);
    } catch (nextError) {
      setError(nextError instanceof Error ? nextError.message : "AI Provider 连接测试失败。");
    } finally {
      setTestingId(null);
    }
  };

  return (
    <section className="settings-section settings-section--ai" aria-labelledby="ai-provider-title">
      <div className="settings-section__icon settings-section__icon--ai">
        <Bot size={16} />
      </div>
      <div className="settings-section__copy ai-provider-settings">
        <div className="ai-provider-settings__title-row">
          <div>
            <h3 id="ai-provider-title">AI Provider</h3>
            <p>供插件的 utools.ai 与 Function Calling 使用；默认不配置云服务，也不会把密钥交给插件 iframe。</p>
          </div>
          <button
            className="settings-action"
            disabled={saving}
            onClick={() => setDraft(emptyDraft())}
            type="button"
          >
            <Plus size={13} />
            添加
          </button>
        </div>

        {!isDesktop() ? (
          <p className="ai-provider-settings__empty">浏览器预览只展示配置入口；请在 Windows 桌面端保存并调用模型。</p>
        ) : loading ? (
          <p className="ai-provider-settings__empty"><LoaderCircle className="spin" size={13} /> 正在读取加密配置…</p>
        ) : profiles.length === 0 ? (
          <p className="ai-provider-settings__empty">尚未配置。可连接本机 Ollama / LM Studio，或用户自己的 HTTPS OpenAI-compatible `/v1` 端点。</p>
        ) : (
          <div className="ai-provider-list">
            {profiles.map((profile) => (
              <article className="ai-provider-card" key={profile.id}>
                <div className="ai-provider-card__mark"><Server size={14} /></div>
                <div className="ai-provider-card__copy">
                  <div>
                    <strong>{profile.label}</strong>
                    {profile.isDefault ? <span>默认</span> : null}
                    {profile.hasApiKey ? <span><KeyRound size={9} /> 密钥已加密</span> : null}
                  </div>
                  <small>{profile.endpoint}</small>
                  <p>{profile.models.length} 个模型 · 默认 {profile.defaultModel}</p>
                </div>
                <div className="ai-provider-card__actions">
                  <button
                    aria-label={`测试 ${profile.label}`}
                    disabled={testingId !== null}
                    onClick={() => void testProvider(profile)}
                    title="测试 /models"
                    type="button"
                  >
                    {testingId === profile.id ? <LoaderCircle className="spin" size={12} /> : <Server size={12} />}
                  </button>
                  <button aria-label={`编辑 ${profile.label}`} onClick={() => setDraft(profileDraft(profile))} type="button">
                    <Pencil size={12} />
                  </button>
                  <button
                    aria-label={confirmDeleteId === profile.id ? `确认删除 ${profile.label}` : `删除 ${profile.label}`}
                    className={confirmDeleteId === profile.id ? "is-confirming" : ""}
                    onBlur={() => setConfirmDeleteId((current) => current === profile.id ? null : current)}
                    onClick={() => void remove(profile)}
                    type="button"
                  >
                    {confirmDeleteId === profile.id ? <Check size={12} /> : <Trash2 size={12} />}
                  </button>
                </div>
              </article>
            ))}
          </div>
        )}

        {draft ? (
          <div className="ai-provider-editor">
            <div className="ai-provider-editor__header">
              <strong>{draft.id ? "编辑 Provider" : "添加 Provider"}</strong>
              <button aria-label="关闭 AI Provider 编辑器" onClick={() => setDraft(null)} type="button"><X size={13} /></button>
            </div>
            <div className="ai-provider-editor__grid">
              <label>
                <span>名称</span>
                <input value={draft.label} onChange={(event) => setDraft({ ...draft, label: event.target.value })} />
              </label>
              <label>
                <span>OpenAI-compatible 端点</span>
                <input
                  spellCheck={false}
                  value={draft.endpoint}
                  onChange={(event) => setDraft({ ...draft, endpoint: event.target.value })}
                />
              </label>
              <label className="ai-provider-editor__models">
                <span>模型 ID（每行一个）</span>
                <textarea
                  spellCheck={false}
                  value={draft.modelsText}
                  onChange={(event) => setDraft({ ...draft, modelsText: event.target.value })}
                />
              </label>
              <label>
                <span>默认模型</span>
                <select value={draft.defaultModel} onChange={(event) => setDraft({ ...draft, defaultModel: event.target.value })}>
                  {draftModelIds.map((id) => <option key={id} value={id}>{id}</option>)}
                </select>
              </label>
              <label>
                <span>API Key</span>
                <input
                  autoComplete="new-password"
                  disabled={draft.removeApiKey}
                  placeholder={draft.hasApiKey ? "留空以保留当前密钥" : "本地无鉴权端点可留空"}
                  type="password"
                  value={draft.apiKey}
                  onChange={(event) => setDraft({ ...draft, apiKey: event.target.value })}
                />
              </label>
            </div>
            <div className="ai-provider-editor__checks">
              <label><input checked={draft.makeDefault} onChange={(event) => setDraft({ ...draft, makeDefault: event.target.checked })} type="checkbox" /> 设为默认 Provider</label>
              {draft.hasApiKey ? (
                <label><input checked={draft.removeApiKey} onChange={(event) => setDraft({ ...draft, removeApiKey: event.target.checked })} type="checkbox" /> 删除现有 API Key</label>
              ) : null}
            </div>
            <div className="ai-provider-editor__footer">
              <small>远程端点必须为 HTTPS；HTTP 仅允许 localhost / 127.0.0.1 / ::1，并禁止重定向。</small>
              <button className="settings-action" disabled={saving || draftModelIds.length === 0} onClick={() => void save()} type="button">
                {saving ? <LoaderCircle className="spin" size={13} /> : <Check size={13} />}
                保存
              </button>
            </div>
          </div>
        ) : null}

        {notice ? <p className="ai-provider-settings__notice"><Check size={12} /> {notice}</p> : null}
        {error ? <p className="settings-error" role="alert">{error}</p> : null}
      </div>
    </section>
  );
}
