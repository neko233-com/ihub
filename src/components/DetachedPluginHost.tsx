import { LoaderCircle, ShieldAlert } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { command, isDesktop, onPluginGlobalShortcut } from "../lib/desktop";
import {
  createDetachedPluginShortcutEvent,
  DETACHED_PLUGIN_BROWSER_PREVIEW_STATUS,
  type DetachedPluginRoute,
} from "../lib/detached-plugin-window";
import type {
  PluginFrontendEvent,
  PluginGlobalShortcutEvent,
  PluginInfo,
} from "../lib/types";
import { PluginFrontendFrame } from "./PluginFrontendFrame";

interface DetachedPluginHostProps {
  route: DetachedPluginRoute;
}

function browserPreviewPlugin(pluginId: string): PluginInfo {
  return {
    id: pluginId,
    name: "插件分离窗口预览",
    version: "browser-preview",
    description: "由 iHub 宿主绘制的安全预览，不加载任何第三方插件代码。",
    enabled: true,
  };
}

export function DetachedPluginRouteError({ message }: { message: string }) {
  return (
    <main className="detached-plugin-host is-error">
      <section aria-labelledby="detached-route-error-title" className="detached-plugin-host__error">
        <ShieldAlert aria-hidden="true" size={24} strokeWidth={1.8} />
        <h1 id="detached-route-error-title">已拒绝分离窗口地址</h1>
        <p>{message}</p>
        <small>iHub 只接受原生宿主从已安装插件 ID 派生出的固定本地地址。</small>
      </section>
    </main>
  );
}

export function DetachedPluginHost({ route }: DetachedPluginHostProps) {
  const desktop = isDesktop();
  const safeBrowserPreview = route.browserPreview && !desktop;
  const previewPlugin = useMemo(
    () => safeBrowserPreview ? browserPreviewPlugin(route.pluginId) : null,
    [route.pluginId, safeBrowserPreview],
  );
  const [plugin, setPlugin] = useState<PluginInfo | null>(previewPlugin);
  const [error, setError] = useState<string | null>(
    !desktop && !safeBrowserPreview
      ? "浏览器只能打开显式的无权限安全预览。"
      : null,
  );
  const [closedPreview, setClosedPreview] = useState(false);
  const [pendingShortcut, setPendingShortcut] =
    useState<PluginGlobalShortcutEvent | null>(null);
  const [pendingEvent, setPendingEvent] = useState<PluginFrontendEvent | null>(null);

  useEffect(() => {
    if (safeBrowserPreview || !desktop) {
      return;
    }
    let active = true;
    void command<PluginInfo>("get_detached_plugin_window_bootstrap")
      .then((resolvedPlugin) => {
        if (!active) {
          return;
        }
        // The query is native-derived, but the registry is authoritative.
        // Refuse rendering if the two identities ever diverge.
        if (resolvedPlugin.id !== route.pluginId) {
          setError("分离窗口路由与宿主登记的插件不一致。");
          return;
        }
        setPlugin(resolvedPlugin);
      })
      .catch((reason) => {
        if (active) {
          setError(
            reason instanceof Error
              ? reason.message
              : "无法验证插件分离窗口。",
          );
        }
      });
    return () => {
      active = false;
    };
  }, [desktop, route.pluginId, safeBrowserPreview]);

  useEffect(() => {
    if (safeBrowserPreview || !desktop) {
      return;
    }
    let disposed = false;
    let stopListening: (() => void) | undefined;
    void onPluginGlobalShortcut((payload) => {
      if (
        !disposed
        && payload.pluginId === route.pluginId
        && payload.commandId !== undefined
        && payload.keyword === undefined
      ) {
        setPendingShortcut(payload);
      }
    }).then((unlisten) => {
      if (disposed) {
        unlisten();
      } else {
        stopListening = unlisten;
      }
    }).catch(() => {
      // The visible plugin surface remains usable if this optional native
      // event channel is unavailable. It never falls back to a global or
      // renderer-selected window target.
    });
    return () => {
      disposed = true;
      stopListening?.();
    };
  }, [desktop, route.pluginId, safeBrowserPreview]);

  useEffect(() => {
    if (!plugin || !pendingShortcut) {
      return;
    }
    setPendingShortcut(null);
    const event = createDetachedPluginShortcutEvent(
      route.pluginId,
      plugin,
      pendingShortcut,
    );
    if (event) {
      setPendingEvent(event);
    }
  }, [pendingShortcut, plugin, route.pluginId]);

  const closeWindow = () => {
    if (safeBrowserPreview) {
      setClosedPreview(true);
      return;
    }
    void command<void>("close_detached_plugin_window").catch((reason) => {
      setError(
        reason instanceof Error ? reason.message : "无法关闭插件分离窗口。",
      );
    });
  };

  if (closedPreview) {
    return (
      <DetachedPluginRouteError message="浏览器安全预览已关闭；没有创建或关闭任何原生窗口。" />
    );
  }

  if (error) {
    return <DetachedPluginRouteError message={error} />;
  }

  if (!plugin) {
    return (
      <main className="detached-plugin-host is-loading" role="status">
        <LoaderCircle aria-hidden="true" className="spin" size={20} />
        正在验证插件与分离窗口身份…
      </main>
    );
  }

  return (
    <main className="detached-plugin-host">
      <PluginFrontendFrame
        browserPreviewStatus={
          safeBrowserPreview ? DETACHED_PLUGIN_BROWSER_PREVIEW_STATUS : undefined
        }
        onClose={closeWindow}
        onPendingEventHandled={(eventId) => {
          setPendingEvent((current) => current?.id === eventId ? null : current);
        }}
        onToast={setError}
        pendingEvent={pendingEvent}
        placement="detached"
        plugin={plugin}
      />
    </main>
  );
}
