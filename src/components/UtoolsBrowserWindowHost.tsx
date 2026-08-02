import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { LoaderCircle, ShieldAlert } from "lucide-react";
import { useEffect, useLayoutEffect, useRef, useState } from "react";
import { command, isDesktop } from "../lib/desktop";
import {
  PluginBridgeInFlightGate,
  isLargePluginBridgeMethod,
  validatePluginBridgeCall,
} from "../lib/plugin-bridge-boundary";
import type { UtoolsBrowserWindowRoute } from "../lib/detached-plugin-window";
import type { PluginFrontendLease, PluginInfo } from "../lib/types";
import { PluginFrontendIframe } from "./PluginFrontendFrame";

const RESPONSE_CHANNEL = "ihub-host-bridge/v1";
const LEASE_HEARTBEAT_MS = 30_000;

interface BrowserBootstrap {
  browserId: string;
  plugin: PluginInfo;
  lease: PluginFrontendLease;
}

interface BrowserNativeMessage {
  browserId: string;
  channel: string;
  args: unknown[];
}

interface BrowserExecuteMessage {
  requestId: string;
  script: string;
}

function validNativeMessage(value: unknown, browserId: string): value is BrowserNativeMessage {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const message = value as Record<string, unknown>;
  return Object.keys(message).every((key) => ["browserId", "channel", "args"].includes(key))
    && message.browserId === browserId
    && typeof message.channel === "string"
    && message.channel.length > 0
    && Array.from(message.channel).length <= 128
    && !/[\u0000-\u001f\u007f]/.test(message.channel)
    && Array.isArray(message.args)
    && message.args.length <= 32;
}

export function UtoolsBrowserWindowHost({ route }: { route: UtoolsBrowserWindowRoute }) {
  const frame = useRef<HTMLIFrameElement>(null);
  const queuedMessages = useRef<BrowserNativeMessage[]>([]);
  const [bootstrap, setBootstrap] = useState<BrowserBootstrap | null>(null);
  const [bridgeReady, setBridgeReady] = useState(false);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(
    isDesktop() ? null : "BrowserWindow 只能由 iHub 桌面宿主创建。",
  );

  useEffect(() => {
    if (!isDesktop()) return;
    let active = true;
    void command<BrowserBootstrap>("get_utools_browser_window_bootstrap")
      .then((value) => {
        if (!active) return;
        if (value.browserId !== route.browserId) {
          setError("BrowserWindow 路由与宿主登记的身份不一致。");
          return;
        }
        setBootstrap(value);
      })
      .catch((reason) => {
        if (active) setError(reason instanceof Error ? reason.message : "无法验证 BrowserWindow。");
      });
    return () => { active = false; };
  }, [route.browserId]);

  useEffect(() => {
    const leaseId = bootstrap?.lease.leaseId;
    if (!leaseId || !isDesktop()) return;
    let active = true;
    const touch = () => {
      void command<boolean>("touch_plugin_frontend_lease", { leaseId })
        .then((current) => {
          if (active && !current) setError("BrowserWindow 插件会话已失效。");
        })
        .catch(() => undefined);
    };
    touch();
    const timer = window.setInterval(touch, LEASE_HEARTBEAT_MS);
    return () => {
      active = false;
      window.clearInterval(timer);
      void command<void>("release_plugin_frontend_url", { leaseId }).catch(() => undefined);
    };
  }, [bootstrap?.lease.leaseId]);

  useLayoutEffect(() => {
    if (!bootstrap) return;
    const { plugin, lease } = bootstrap;
    let active = true;
    const inFlight = new PluginBridgeInFlightGate();
    const onMessage = (event: MessageEvent<unknown>) => {
      const target = frame.current?.contentWindow;
      if (!target || event.source !== target || event.origin !== lease.origin) return;
      const validation = validatePluginBridgeCall(event.data, plugin.id);
      if (!validation.ok) return;
      const bridgeCall = validation.call;
      const admission = inFlight.begin(
        bridgeCall.id,
        isLargePluginBridgeMethod(bridgeCall.request.method),
      );
      if (admission !== "accepted") return;
      const reply = (payload: Record<string, unknown>) => {
        try {
          if (active) target.postMessage(payload, lease.origin);
        } finally {
          inFlight.finish(bridgeCall.id);
        }
      };
      void command<unknown>("plugin_host_call", {
        request: {
          pluginId: plugin.id,
          leaseId: lease.leaseId,
          surface: false,
          interactionId: bridgeCall.request.interactionId,
          method: bridgeCall.request.method,
          params: bridgeCall.request.params,
        },
      }).then((result) => reply({
        channel: RESPONSE_CHANNEL,
        type: "response",
        id: bridgeCall.id,
        ok: true,
        result,
      })).catch((reason) => reply({
        channel: RESPONSE_CHANNEL,
        type: "response",
        id: bridgeCall.id,
        ok: false,
        error: reason instanceof Error ? reason.message : "BrowserWindow host request failed.",
      }));
    };
    window.addEventListener("message", onMessage);
    setBridgeReady(true);
    return () => {
      active = false;
      inFlight.clear();
      window.removeEventListener("message", onMessage);
      setBridgeReady(false);
    };
  }, [bootstrap]);

  useEffect(() => {
    if (!bootstrap) return;
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    void listen<unknown>("ihub://utools-browser/child-message", (event) => {
      if (disposed || !validNativeMessage(event.payload, bootstrap.browserId)) return;
      const target = frame.current?.contentWindow;
      if (!bridgeReady || !target) {
        if (queuedMessages.current.length < 64) queuedMessages.current.push(event.payload);
        return;
      }
      target.postMessage({
        channel: RESPONSE_CHANNEL,
        type: "event",
        name: `ihub://plugin/${bootstrap.plugin.id}/event/utools.browser.ipc`,
        payload: event.payload,
      }, bootstrap.lease.origin);
    }).then((stop) => {
      if (disposed) stop(); else unlisten = stop;
    }).catch(() => undefined);
    return () => { disposed = true; unlisten?.(); };
  }, [bootstrap, bridgeReady]);

  useEffect(() => {
    if (!bootstrap) return;
    let disposed = false;
    let unlisten: UnlistenFn | undefined;
    void listen<unknown>("ihub://utools-browser/execute", (event) => {
      if (disposed || !event.payload || typeof event.payload !== "object" || Array.isArray(event.payload)) return;
      const payload = event.payload as Partial<BrowserExecuteMessage>;
      if (
        typeof payload.requestId !== "string"
        || typeof payload.script !== "string"
        || payload.script.length === 0
        || Array.from(payload.script).length > 65_536
      ) return;
      const target = frame.current?.contentWindow;
      if (!target || !bridgeReady) return;
      target.postMessage({
        channel: RESPONSE_CHANNEL,
        type: "event",
        name: `ihub://plugin/${bootstrap.plugin.id}/event/utools.browser.execute`,
        payload,
      }, bootstrap.lease.origin);
    }).then((stop) => {
      if (disposed) stop(); else unlisten = stop;
    }).catch(() => undefined);
    return () => { disposed = true; unlisten?.(); };
  }, [bootstrap, bridgeReady]);

  useEffect(() => {
    if (!bootstrap || !bridgeReady || queuedMessages.current.length === 0) return;
    const target = frame.current?.contentWindow;
    if (!target) return;
    for (const payload of queuedMessages.current.splice(0)) {
      target.postMessage({
        channel: RESPONSE_CHANNEL,
        type: "event",
        name: `ihub://plugin/${bootstrap.plugin.id}/event/utools.browser.ipc`,
        payload,
      }, bootstrap.lease.origin);
    }
  }, [bootstrap, bridgeReady]);

  if (error) {
    return (
      <main className="utools-browser-host is-error" role="alert">
        <ShieldAlert aria-hidden="true" size={24} />
        <p>BrowserWindow 已拒绝加载</p>
        <small>{error}</small>
      </main>
    );
  }
  if (!bootstrap || !bridgeReady) {
    return (
      <main className="utools-browser-host is-loading" role="status">
        <LoaderCircle aria-hidden="true" className="spin" size={20} />
        正在验证 BrowserWindow…
      </main>
    );
  }
  return (
    <main className="utools-browser-host">
      {!loaded ? (
        <div className="utools-browser-host__loading" role="status">
          <LoaderCircle aria-hidden="true" className="spin" size={20} />
        </div>
      ) : null}
      <PluginFrontendIframe
        allowDisplayCapture={false}
        allowMicrophone={false}
        frameRef={frame}
        onError={() => setError("BrowserWindow 插件页面无法加载。")}
        onLoad={() => {
          setLoaded(true);
          void command<void>("mark_utools_browser_window_ready", {
            leaseId: bootstrap.lease.leaseId,
          }).catch((reason) => setError(
            reason instanceof Error ? reason.message : "BrowserWindow 就绪校验失败。",
          ));
        }}
        purpose="runtime"
        sourceUrl={bootstrap.lease.url}
        title={`${bootstrap.plugin.name} BrowserWindow`}
      />
    </main>
  );
}
