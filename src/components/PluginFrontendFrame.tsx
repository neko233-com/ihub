import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { AnimatePresence, motion } from "motion/react";
import { ChevronLeft, LoaderCircle, Puzzle, ShieldCheck } from "lucide-react";
import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { command, isDesktop } from "../lib/desktop";
import type { PluginFrontendEvent, PluginFrontendLease, PluginInfo } from "../lib/types";

const REQUEST_CHANNEL = "ihub-plugin-bridge/v1";
const RESPONSE_CHANNEL = "ihub-host-bridge/v1";
const LEASE_HEARTBEAT_MS = 30_000;

interface PluginFrontendFrameProps {
  plugin: PluginInfo | null;
  pendingEvent: PluginFrontendEvent | null;
  onClose: () => void;
  onPendingEventHandled: (eventId: string) => void;
  onToast: (message: string) => void;
  /** A hidden runtime hosts only manifest-declared search providers. It keeps
   * the exact same postMessage bridge as the visible plugin surface. */
  mode?: "surface" | "runtime";
  /** Called once a visible surface has completed lifecycle.ready and its
   * native command-event subscription is listening. This lets the trusted
   * parent issue a user-confirmed one-shot launcher context only after the
   * host can accept and route the exact registered frontend command. */
  onSurfaceReady?: (pluginId: string, leaseId: string) => void;
  /** A failed, expired, or replaced visible lease must let the parent discard
   * any pending or already-dispatched one-shot launcher context for it. */
  onSurfaceUnavailable?: (pluginId: string, leaseId?: string) => void;
  onRuntimeReady?: (pluginId: string) => void;
  onRuntimeDisposed?: (pluginId: string) => void;
  onSearchProviderRegistered?: (pluginId: string, providerId: string) => void;
  onSearchProviderUnregistered?: (pluginId: string, providerId: string) => void;
}

interface HostBridgeEvent {
  name: string;
  payload: unknown;
}

interface PluginFrontendSource extends PluginFrontendLease {
  /** Renderer-only identity that prevents a previous plugin's iframe from
   * briefly rendering while React is switching sources. */
  pluginId: string;
}

interface BridgeCall {
  channel: typeof REQUEST_CHANNEL;
  type: "call";
  id: string;
  request: {
    method: string;
    params?: unknown;
  };
}

interface PendingCursorColorRequest {
  id: string;
  pluginId: string;
  leaseId: string;
  reply: (payload: Record<string, unknown>) => void;
}

interface CursorColorApproval {
  approvalId: string;
}

function isBridgeCall(value: unknown): value is BridgeCall {
  if (!value || typeof value !== "object") {
    return false;
  }
  const message = value as Record<string, unknown>;
  const request = message.request;
  return (
    message.channel === REQUEST_CHANNEL &&
    message.type === "call" &&
    typeof message.id === "string" &&
    Boolean(request) &&
    typeof request === "object" &&
    typeof (request as Record<string, unknown>).method === "string"
  );
}

/**
 * List refreshes create new object identities, but must not reload a live
 * plugin iframe. Only data that can alter the resolved frontend source or its
 * lifecycle state participates in a new source lease.
 */
function frontendSourceKey(plugin: PluginInfo | null): string | null {
  if (!plugin) {
    return null;
  }
  return [
    plugin.id,
    plugin.enabled === false ? "disabled" : "enabled",
    plugin.frontendEntry ?? "",
    plugin.commit ?? "",
    plugin.installedAt ?? "",
    plugin.sourceLock?.resolvedCommit ?? "",
    plugin.sourceLock?.installedAt ?? "",
    plugin.isDevelopmentLink ? "local" : "managed",
    plugin.localPath ?? "",
  ].join("\u0000");
}

export function PluginFrontendFrame({
  plugin,
  pendingEvent,
  onClose,
  onPendingEventHandled,
  onToast,
  mode = "surface",
  onSurfaceReady,
  onSurfaceUnavailable,
  onRuntimeReady,
  onRuntimeDisposed,
  onSearchProviderRegistered,
  onSearchProviderUnregistered,
}: PluginFrontendFrameProps) {
  const frame = useRef<HTMLIFrameElement>(null);
  const queuedHostEvents = useRef<HostBridgeEvent[]>([]);
  const dispatchedPendingEvents = useRef(new Set<string>());
  const registeredSearchProviders = useRef(new Set<string>());
  const previousPluginSource = useRef<{ pluginId: string; key: string } | null>(null);
  const [source, setSource] = useState<PluginFrontendSource | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [readyPluginId, setReadyPluginId] = useState<string | null>(null);
  const [commandEventSubscriptionLeaseId, setCommandEventSubscriptionLeaseId] = useState<string | null>(null);
  const [bridgeReadyLeaseId, setBridgeReadyLeaseId] = useState<string | null>(null);
  const [leaseRetry, setLeaseRetry] = useState(0);
  const [pendingCursorColorRequest, setPendingCursorColorRequest] = useState<PendingCursorColorRequest | null>(null);
  const [approvingCursorColor, setApprovingCursorColor] = useState(false);
  const readyPluginIdRef = useRef<string | null>(null);
  const postEventToFrameRef = useRef<(name: string, payload: unknown) => boolean>(() => false);
  const pendingCursorColorRequestRef = useRef<PendingCursorColorRequest | null>(null);
  const announcedSurfaceReadyLeaseRef = useRef<string | null>(null);
  const pluginId = plugin?.id ?? null;
  const pluginSourceKey = frontendSourceKey(plugin);
  const runtimeOnly = mode === "runtime";
  const sourceIsCurrent = source?.pluginId === pluginId;
  const sourceOrigin = sourceIsCurrent ? source?.origin ?? null : null;
  const sourceLeaseId = sourceIsCurrent ? source?.leaseId ?? null : null;
  const bridgeIsReady = sourceIsCurrent && bridgeReadyLeaseId === sourceLeaseId;
  const postEventToFrame = useCallback((name: string, payload: unknown) => {
    const target = frame.current?.contentWindow;
    if (!target || !sourceOrigin || !bridgeIsReady) {
      return false;
    }

    target.postMessage(
      {
        channel: RESPONSE_CHANNEL,
        type: "event",
        name,
        payload,
      },
      sourceOrigin,
    );
    return true;
  }, [bridgeIsReady, sourceOrigin]);

  useEffect(() => {
    postEventToFrameRef.current = postEventToFrame;
  }, [postEventToFrame]);

  const clearPendingCursorColorRequest = useCallback((request: PendingCursorColorRequest) => {
    if (pendingCursorColorRequestRef.current !== request) {
      return;
    }
    pendingCursorColorRequestRef.current = null;
    setPendingCursorColorRequest(null);
    setApprovingCursorColor(false);
  }, []);

  const cancelPendingCursorColorRequest = useCallback(() => {
    const request = pendingCursorColorRequestRef.current;
    if (!request || approvingCursorColor) {
      return;
    }
    request.reply({
      channel: RESPONSE_CHANNEL,
      type: "response",
      id: request.id,
      ok: false,
      error: "Cursor color sampling was cancelled in the iHub host.",
    });
    clearPendingCursorColorRequest(request);
  }, [approvingCursorColor, clearPendingCursorColorRequest]);

  const approvePendingCursorColorRequest = useCallback(() => {
    const request = pendingCursorColorRequestRef.current;
    if (!request || approvingCursorColor) {
      return;
    }
    setApprovingCursorColor(true);
    void (async () => {
      try {
        // This token is issued only after the trusted host overlay's click. It
        // never travels to the iframe: the parent injects it into the next
        // host call and forwards only the color result back to the plugin.
        const approval = await command<CursorColorApproval>("issue_plugin_cursor_color_approval", {
          pluginId: request.pluginId,
          leaseId: request.leaseId,
        });
        const result = await command<unknown>("plugin_host_call", {
          request: {
            pluginId: request.pluginId,
            leaseId: request.leaseId,
            surface: true,
            method: "cursorColor.sampleOnce",
            params: { approvalId: approval.approvalId },
          },
        });
        request.reply({
          channel: RESPONSE_CHANNEL,
          type: "response",
          id: request.id,
          ok: true,
          result,
        });
      } catch (reason) {
        request.reply({
          channel: RESPONSE_CHANNEL,
          type: "response",
          id: request.id,
          ok: false,
          error: reason instanceof Error ? reason.message : "iHub could not sample the cursor color.",
        });
      } finally {
        clearPendingCursorColorRequest(request);
      }
    })();
  }, [approvingCursorColor, clearPendingCursorColorRequest]);

  useEffect(() => {
    const request = pendingCursorColorRequestRef.current;
    if (!request || request.leaseId === sourceLeaseId) {
      return;
    }
    request.reply({
      channel: RESPONSE_CHANNEL,
      type: "response",
      id: request.id,
      ok: false,
      error: "The plugin surface changed before cursor color sampling was confirmed.",
    });
    clearPendingCursorColorRequest(request);
  }, [clearPendingCursorColorRequest, sourceLeaseId]);

  useEffect(() => {
    const previous = previousPluginSource.current;
    if (
      previous
      && previous.key !== pluginSourceKey
    ) {
      // A source replacement has the same plugin ID, so the normal unmount
      // cleanup does not run. Clear provider readiness before a newly leased
      // iframe starts registering its own handlers.
      onRuntimeDisposed?.(previous.pluginId);
    }
    queuedHostEvents.current = [];
    dispatchedPendingEvents.current.clear();
    registeredSearchProviders.current.clear();
    setReadyPluginId(null);
    setCommandEventSubscriptionLeaseId(null);
    announcedSurfaceReadyLeaseRef.current = null;
    previousPluginSource.current = pluginId && pluginSourceKey
      ? { pluginId, key: pluginSourceKey }
      : null;
  }, [onRuntimeDisposed, pluginId, pluginSourceKey]);

  useEffect(() => {
    readyPluginIdRef.current = readyPluginId;
  }, [readyPluginId]);

  useEffect(() => {
    if (!pluginId || readyPluginId !== pluginId) {
      return;
    }
    onRuntimeReady?.(pluginId);
    for (const providerId of registeredSearchProviders.current) {
      onSearchProviderRegistered?.(pluginId, providerId);
    }
  }, [onRuntimeReady, onSearchProviderRegistered, pluginId, readyPluginId]);

  useEffect(() => {
    if (
      runtimeOnly
      || !pluginId
      || !sourceLeaseId
      || readyPluginId !== pluginId
      || commandEventSubscriptionLeaseId !== sourceLeaseId
    ) {
      return;
    }
    const readyKey = `${pluginId}:${sourceLeaseId}`;
    if (announcedSurfaceReadyLeaseRef.current === readyKey) {
      return;
    }
    announcedSurfaceReadyLeaseRef.current = readyKey;
    onSurfaceReady?.(pluginId, sourceLeaseId);
  }, [
    commandEventSubscriptionLeaseId,
    onSurfaceReady,
    pluginId,
    readyPluginId,
    runtimeOnly,
    sourceLeaseId,
  ]);

  useEffect(() => {
    if (!pluginId) {
      return;
    }
    return () => {
      // Do not asynchronously send lifecycle.dispose from React cleanup: an
      // old iframe's delayed IPC could otherwise erase a newer iframe's
      // registration for the same plugin. A well-behaved SDK runtime still
      // sends lifecycle.dispose itself; this callback only invalidates UI-side
      // readiness so a replacement must register again before querying.
      onRuntimeDisposed?.(pluginId);
    };
  }, [onRuntimeDisposed, pluginId]);

  useEffect(() => {
    let alive = true;
    let activeLeaseId: string | null = null;
    setSource(null);
    setBridgeReadyLeaseId(null);
    setError(null);

    if (!pluginId || !pluginSourceKey) {
      return () => {
        alive = false;
      };
    }
    if (!isDesktop()) {
      setError("插件前端只能在 iHub 桌面端加载。");
      return () => {
        alive = false;
      };
    }

    setLoading(true);
    void command<PluginFrontendLease>("get_plugin_frontend_url", {
      pluginId,
      purpose: runtimeOnly ? "runtime" : "surface",
    })
      .then((lease) => {
        if (alive) {
          activeLeaseId = lease.leaseId;
          setSource({ ...lease, pluginId });
        } else {
          void command<void>("release_plugin_frontend_url", { leaseId: lease.leaseId })
            .catch(() => undefined);
        }
      })
      .catch((reason) => {
        if (alive) {
          setError(reason instanceof Error ? reason.message : "无法解析插件前端入口。");
          setLoading(false);
        }
      });

    return () => {
      alive = false;
      if (activeLeaseId) {
        void command<void>("release_plugin_frontend_url", { leaseId: activeLeaseId })
          .catch(() => undefined);
      }
    };
  }, [leaseRetry, pluginId, pluginSourceKey, runtimeOnly]);

  useEffect(() => {
    if (!sourceLeaseId || !isDesktop()) {
      return;
    }

    let active = true;
    const touch = () => {
      void command<boolean>("touch_plugin_frontend_lease", { leaseId: sourceLeaseId })
        .then((leaseIsActive) => {
          if (active && !leaseIsActive) {
            // Native state can disappear if the renderer reloads late or a
            // lifecycle change revokes this source. Reacquire rather than
            // leaving a visible iframe attached to an expired lease.
            if (!runtimeOnly && pluginId) {
              onSurfaceUnavailable?.(pluginId, sourceLeaseId);
            }
            setSource((current) => (current?.leaseId === sourceLeaseId ? null : current));
            setBridgeReadyLeaseId((current) => (
              current === sourceLeaseId ? null : current
            ));
            setLeaseRetry((current) => current + 1);
          }
        })
        .catch(() => undefined);
    };

    touch();
    const timer = window.setInterval(touch, LEASE_HEARTBEAT_MS);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, [onSurfaceUnavailable, pluginId, runtimeOnly, sourceLeaseId]);

  useLayoutEffect(() => {
    if (!pluginId || !sourceOrigin || !sourceLeaseId) {
      return;
    }

    // The iframe is rendered only after this layout effect has installed its
    // listener and marked the lease ready. That avoids losing an SDK's first
    // register/ready call when a loopback document loads immediately.
    let bridgeActive = true;

    const onMessage = (event: MessageEvent<unknown>) => {
      const sourceWindow = frame.current?.contentWindow;
      if (
        !sourceWindow
        || event.source !== sourceWindow
        || event.origin !== sourceOrigin
        || !isBridgeCall(event.data)
      ) {
        return;
      }

      const reply = (payload: Record<string, unknown>) => {
        if (bridgeActive) {
          // Capture the WindowProxy that sent this request. A delayed response
          // must never be redirected to whatever iframe React mounts later.
          sourceWindow.postMessage(payload, sourceOrigin);
        }
      };

      const bridgeCall = event.data;
      if (bridgeCall.request.method === "cursorColor.sampleOnce") {
        if (runtimeOnly) {
          reply({
            channel: RESPONSE_CHANNEL,
            type: "response",
            id: bridgeCall.id,
            ok: false,
            error: "Cursor color sampling is unavailable from a hidden plugin runtime.",
          });
          return;
        }
        if (pendingCursorColorRequestRef.current) {
          reply({
            channel: RESPONSE_CHANNEL,
            type: "response",
            id: bridgeCall.id,
            ok: false,
            error: "Another cursor color confirmation is already waiting in iHub.",
          });
          return;
        }
        const pendingRequest: PendingCursorColorRequest = {
          id: bridgeCall.id,
          pluginId,
          leaseId: sourceLeaseId,
          reply,
        };
        // Do not forward a raw sampling request through the ordinary bridge.
        // The iframe waits for the parent-owned confirmation instead.
        pendingCursorColorRequestRef.current = pendingRequest;
        setPendingCursorColorRequest(pendingRequest);
        return;
      }
      const providerId = bridgeCall.request.method === "search.register"
        && bridgeCall.request.params
        && typeof bridgeCall.request.params === "object"
        && !Array.isArray(bridgeCall.request.params)
        ? (bridgeCall.request.params as { definition?: { id?: unknown } }).definition?.id
        : undefined;
      const unregisteredProviderId = bridgeCall.request.method === "search.unregister"
        && bridgeCall.request.params
        && typeof bridgeCall.request.params === "object"
        && !Array.isArray(bridgeCall.request.params)
        ? (() => {
            const params = bridgeCall.request.params as { providerId?: unknown; id?: unknown };
            return typeof params.providerId === "string"
              ? params.providerId
              : typeof params.id === "string"
                ? params.id
                : undefined;
          })()
        : undefined;
      const request = {
        pluginId,
        // The iframe never controls this value. It binds the host request to
        // the lease that produced the current source URL, so a revoked old
        // document cannot continue using the Bridge after an update/link.
        leaseId: sourceLeaseId,
        // The renderer, not the iframe payload, declares the frontend role.
        // Native code separately verifies the lease's purpose for the only
        // user-presence-sensitive bridge method.
        surface: !runtimeOnly,
        method: bridgeCall.request.method,
        params: bridgeCall.request.params,
      };

      void command<unknown>("plugin_host_call", { request })
        .then((result) => {
          if (!bridgeActive) {
            return;
          }
          reply({
            channel: RESPONSE_CHANNEL,
            type: "response",
            id: bridgeCall.id,
            ok: true,
            result,
          });
          if (bridgeCall.request.method === "lifecycle.ready") {
            // Runtime.activate() only makes this call after its handlers have
            // registered. Delay host-delivered work until that handshake is
            // accepted, otherwise an iframe can miss its first invocation.
            setReadyPluginId(pluginId);
          }
          if (bridgeCall.request.method === "lifecycle.dispose") {
            setReadyPluginId((current) => (current === pluginId ? null : current));
            onRuntimeDisposed?.(pluginId);
          }
          if (bridgeCall.request.method === "search.register" && typeof providerId === "string") {
            registeredSearchProviders.current.add(providerId);
            if (readyPluginIdRef.current === pluginId) {
              onSearchProviderRegistered?.(pluginId, providerId);
            }
          }
          if (
            bridgeCall.request.method === "search.unregister"
            && typeof unregisteredProviderId === "string"
          ) {
            registeredSearchProviders.current.delete(unregisteredProviderId);
            onSearchProviderUnregistered?.(pluginId, unregisteredProviderId);
          }
        })
        .catch((reason) => {
          if (!bridgeActive) {
            return;
          }
          reply({
            channel: RESPONSE_CHANNEL,
            type: "response",
            id: bridgeCall.id,
            ok: false,
            error: reason instanceof Error ? reason.message : "Host request failed.",
          });
        });
    };

    window.addEventListener("message", onMessage);
    setBridgeReadyLeaseId(sourceLeaseId);
    return () => {
      bridgeActive = false;
      window.removeEventListener("message", onMessage);
    };
  }, [
    onRuntimeDisposed,
    onSearchProviderRegistered,
    onSearchProviderUnregistered,
    pluginId,
    runtimeOnly,
    sourceLeaseId,
    sourceOrigin,
  ]);

  useEffect(() => {
    if (!pluginId || !sourceLeaseId || !isDesktop()) {
      return;
    }

    let disposed = false;
    const unlisten: UnlistenFn[] = [];
    const forward = (name: string, payload: unknown) => {
      if (readyPluginIdRef.current === pluginId && postEventToFrameRef.current(name, payload)) {
        return;
      }
      queuedHostEvents.current.push({ name, payload });
    };
    const subscribe = async (kind: "command" | "search") => {
      try {
        const stop = await listen<unknown>(`ihub://plugin/${pluginId}/${kind}`, (event) => {
          forward(`ihub://plugin/${pluginId}/${kind}`, event.payload);
        });
        if (disposed) {
          stop();
        } else {
          unlisten.push(stop);
          if (kind === "command") {
            // `invoke_plugin_frontend_command` emits through this native
            // listener. Do not tell the parent a plugin is dispatch-ready
            // until both this subscription and lifecycle.ready exist.
            setCommandEventSubscriptionLeaseId(sourceLeaseId);
          }
        }
      } catch {
        // Desktop event forwarding is additive. A plugin can still render
        // normally if its host has not exposed a particular event channel.
      }
    };

    void Promise.all([subscribe("command"), subscribe("search")]);
    return () => {
      disposed = true;
      unlisten.forEach((stop) => stop());
      setCommandEventSubscriptionLeaseId((current) => current === sourceLeaseId ? null : current);
    };
  }, [pluginId, sourceLeaseId]);

  useEffect(() => {
    if (!pluginId || readyPluginId !== pluginId || queuedHostEvents.current.length === 0) {
      return;
    }

    const events = queuedHostEvents.current;
    queuedHostEvents.current = [];
    for (const event of events) {
      if (!postEventToFrame(event.name, event.payload)) {
        queuedHostEvents.current.unshift(event);
        break;
      }
    }
  }, [pluginId, postEventToFrame, readyPluginId]);

  useEffect(() => {
    if (
      !pendingEvent ||
      pendingEvent.pluginId !== pluginId ||
      readyPluginId !== pluginId ||
      dispatchedPendingEvents.current.has(pendingEvent.id)
    ) {
      return;
    }

    if (!postEventToFrame(pendingEvent.name, pendingEvent.payload)) {
      return;
    }
    dispatchedPendingEvents.current.add(pendingEvent.id);
    onPendingEventHandled(pendingEvent.id);
  }, [onPendingEventHandled, pendingEvent, pluginId, postEventToFrame, readyPluginId]);

  useEffect(() => {
    if (!error || runtimeOnly) {
      return;
    }
    if (pluginId) {
      onSurfaceUnavailable?.(pluginId, sourceLeaseId ?? undefined);
    }
    onToast(error);
  }, [error, onSurfaceUnavailable, onToast, pluginId, runtimeOnly, sourceLeaseId]);

  if (runtimeOnly) {
    return source && bridgeIsReady ? (
      <iframe
        aria-hidden="true"
        className="plugin-search-runtime-frame"
        onError={() => {
          setLoading(false);
          setError("插件前端页面无法加载。");
        }}
        onLoad={() => setLoading(false)}
        ref={frame}
        referrerPolicy="no-referrer"
        src={source.url}
        tabIndex={-1}
        title={plugin ? `${plugin.name} search runtime` : "plugin search runtime"}
      />
    ) : null;
  }

  return (
    <AnimatePresence>
      {plugin ? (
        <motion.section
          aria-label={`${plugin.name} 插件界面`}
          className="plugin-frame-overlay"
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ type: "spring", stiffness: 420, damping: 36 }}
        >
          <header className="plugin-frame__header">
            <div className="plugin-frame__identity">
              <button
                aria-label="返回 iHub 启动器"
                className="plugin-frame__back"
                onClick={onClose}
                title="返回 iHub"
                type="button"
              >
                <ChevronLeft aria-hidden="true" size={16} strokeWidth={2.1} />
                <span>返回</span>
              </button>
              <span className="plugin-frame__tag">
                <span aria-hidden="true" className="plugin-frame__tag-icon">
                  <Puzzle size={15} strokeWidth={1.9} />
                </span>
                <h1>{plugin.name}</h1>
              </span>
            </div>
            <span
              aria-label="安全状态：插件界面已隔离加载"
              className="plugin-frame__security"
              title="插件界面在独立来源中运行，只能通过受限桥接访问宿主能力。"
            >
              <ShieldCheck aria-hidden="true" size={14} strokeWidth={1.9} />
              <span>隔离加载</span>
            </span>
          </header>

          <div className="plugin-frame__content">
            {pendingCursorColorRequest ? (
              <div
                aria-describedby="plugin-cursor-confirm-copy"
                aria-labelledby="plugin-cursor-confirm-title"
                aria-modal="true"
                className="plugin-frame__cursor-confirm"
                role="dialog"
              >
                <div className="plugin-frame__cursor-confirm-card">
                  <span className="plugin-frame__cursor-confirm-eyebrow">SYSTEM COLOR PICKER</span>
                  <h2 id="plugin-cursor-confirm-title">允许读取一个光标像素？</h2>
                  <p id="plugin-cursor-confirm-copy">
                    {plugin.name} 将在确认后等待 2 秒，读取鼠标下的一个颜色值。
                    不会截屏、记录坐标或访问其他应用窗口。
                  </p>
                  <div className="plugin-frame__cursor-confirm-actions">
                    <button
                      className="plugin-frame__cursor-confirm-cancel"
                      disabled={approvingCursorColor}
                      onClick={cancelPendingCursorColorRequest}
                      type="button"
                    >
                      取消
                    </button>
                    <button
                      className="plugin-frame__cursor-confirm-approve"
                      disabled={approvingCursorColor}
                      onClick={approvePendingCursorColorRequest}
                      type="button"
                    >
                      {approvingCursorColor ? "正在取色…" : "开始取色"}
                    </button>
                  </div>
                </div>
              </div>
            ) : null}
            {loading ? (
              <div className="plugin-frame__loading">
                <LoaderCircle className="spin" size={20} />
                正在校验插件前端入口…
              </div>
            ) : null}
            {error ? (
              <div className="plugin-frame__error">
                <p>无法载入此插件前端。</p>
                <small>{error}</small>
              </div>
            ) : null}
            {source && bridgeIsReady ? (
              <iframe
                key={`${plugin.id}:${source.leaseId}`}
                onError={() => {
                  setLoading(false);
                  setError("插件前端页面无法加载。");
                }}
                onLoad={() => setLoading(false)}
                ref={frame}
                referrerPolicy="no-referrer"
                src={source.url}
                title={plugin.name + " plugin frontend"}
              />
            ) : null}
          </div>
        </motion.section>
      ) : null}
    </AnimatePresence>
  );
}
