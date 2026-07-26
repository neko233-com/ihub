import { convertFileSrc } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { AnimatePresence, motion } from "motion/react";
import { ExternalLink, LoaderCircle, Puzzle, ShieldCheck, X } from "lucide-react";
import { useCallback, useEffect, useRef, useState } from "react";
import { command, isDesktop } from "../lib/desktop";
import type { PluginFrontendEvent, PluginInfo } from "../lib/types";

const REQUEST_CHANNEL = "ihub-plugin-bridge/v1";
const RESPONSE_CHANNEL = "ihub-host-bridge/v1";

interface PluginFrontendFrameProps {
  plugin: PluginInfo | null;
  pendingEvent: PluginFrontendEvent | null;
  onClose: () => void;
  onPendingEventHandled: (eventId: string) => void;
  onToast: (message: string) => void;
}

interface HostBridgeEvent {
  name: string;
  payload: unknown;
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

export function PluginFrontendFrame({
  plugin,
  pendingEvent,
  onClose,
  onPendingEventHandled,
  onToast,
}: PluginFrontendFrameProps) {
  const frame = useRef<HTMLIFrameElement>(null);
  const queuedHostEvents = useRef<HostBridgeEvent[]>([]);
  const dispatchedPendingEvents = useRef(new Set<string>());
  const [source, setSource] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [readyPluginId, setReadyPluginId] = useState<string | null>(null);
  const pluginId = plugin?.id ?? null;

  const postEventToFrame = useCallback((name: string, payload: unknown) => {
    const target = frame.current?.contentWindow;
    if (!target) {
      return false;
    }

    target.postMessage(
      {
        channel: RESPONSE_CHANNEL,
        type: "event",
        name,
        payload,
      },
      "*",
    );
    return true;
  }, []);

  useEffect(() => {
    queuedHostEvents.current = [];
    dispatchedPendingEvents.current.clear();
    setReadyPluginId(null);
  }, [pluginId]);

  useEffect(() => {
    let alive = true;
    setSource(null);
    setError(null);

    if (!plugin) {
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
    void command<string>("get_plugin_frontend_path", { pluginId: plugin.id })
      .then((path) => {
        if (alive) {
          setSource(convertFileSrc(path));
        }
      })
      .catch((reason) => {
        if (alive) {
          setError(reason instanceof Error ? reason.message : "无法解析插件前端入口。");
        }
      })
      .finally(() => {
        if (alive) {
          setLoading(false);
        }
      });

    return () => {
      alive = false;
    };
  }, [plugin]);

  useEffect(() => {
    if (!pluginId) {
      return;
    }

    const reply = (payload: Record<string, unknown>) => {
      frame.current?.contentWindow?.postMessage(payload, "*");
    };

    const onMessage = (event: MessageEvent<unknown>) => {
      if (event.source !== frame.current?.contentWindow || !isBridgeCall(event.data)) {
        return;
      }

      const bridgeCall = event.data;
      const request = {
        pluginId,
        method: bridgeCall.request.method,
        params: bridgeCall.request.params,
      };

      void command<unknown>("plugin_host_call", { request })
        .then((result) => {
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
          }
        })
        .catch((reason) => {
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
    return () => window.removeEventListener("message", onMessage);
  }, [pluginId]);

  useEffect(() => {
    if (!pluginId || !isDesktop()) {
      return;
    }

    let disposed = false;
    const unlisten: UnlistenFn[] = [];
    const forward = (name: string, payload: unknown) => {
      if (readyPluginId === pluginId && postEventToFrame(name, payload)) {
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
    };
  }, [pluginId, postEventToFrame, readyPluginId]);

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
    if (!error) {
      return;
    }
    onToast(error);
  }, [error, onToast]);

  return (
    <AnimatePresence>
      {plugin ? (
        <motion.section
          aria-label={plugin.name + " plugin view"}
          className="plugin-frame-overlay"
          initial={{ opacity: 0, y: 18 }}
          animate={{ opacity: 1, y: 0 }}
          exit={{ opacity: 0, y: 18 }}
          transition={{ type: "spring", stiffness: 420, damping: 36 }}
        >
          <header className="plugin-frame__header">
            <div className="plugin-frame__identity">
              <span className="plugin-row__glyph">
                <Puzzle size={16} />
              </span>
              <div>
                <small>PLUGIN FRONTEND</small>
                <strong>{plugin.name}</strong>
              </div>
            </div>
            <div className="plugin-frame__actions">
              <span className="plugin-frame__bridge">
                <ShieldCheck size={14} />
                iHub Bridge
              </span>
              <button aria-label="Close plugin view" className="icon-button" onClick={onClose}>
                <X size={18} />
              </button>
            </div>
          </header>

          <div className="plugin-frame__content">
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
            {source ? (
              <iframe
                key={plugin.id}
                onLoad={() => setLoading(false)}
                ref={frame}
                src={source}
                title={plugin.name + " plugin frontend"}
              />
            ) : null}
          </div>

          <footer className="plugin-frame__footer">
            <span>
              仅暴露声明的 iHub Bridge；插件不直接访问 Tauri API。
            </span>
            <span>
              <ExternalLink size={13} />
              {plugin.frontendEntry ?? "frontend entry"}
            </span>
          </footer>
        </motion.section>
      ) : null}
    </AnimatePresence>
  );
}
