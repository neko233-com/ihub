import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { AnimatePresence, motion } from "motion/react";
import {
  ChevronLeft,
  ExternalLink,
  LoaderCircle,
  Puzzle,
  Search,
  ShieldCheck,
  X,
} from "lucide-react";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type RefObject,
} from "react";
import { command, isDesktop } from "../lib/desktop";
import { shouldDetachPluginSurface } from "../lib/detached-plugin-window";
import {
  PluginBridgeInFlightGate,
  isLargePluginBridgeMethod,
  validatePluginBridgeCall,
} from "../lib/plugin-bridge-boundary";
import {
  enqueueBoundedPluginHostEvent,
  rememberBoundedPluginEventId,
  restoreFailedPluginHostEventTail,
} from "../lib/plugin-host-event-queue";
import {
  PLUGIN_SUB_INPUT_MAX_VALUE_LENGTH,
  resolvePluginSubInputBridgeCall,
  type PluginSubInputHostState,
} from "../lib/plugin-sub-input";
import type { PluginFrontendEvent, PluginFrontendLease, PluginInfo } from "../lib/types";
import {
  validateRegionCaptureSize,
  type CroppedCapture,
  type RegionCaptureSource,
} from "../lib/region-capture";
import { PluginArtwork, safePluginArtworkSrc } from "./PluginArtwork";
import { RegionCaptureEditor } from "./RegionCaptureEditor";

const RESPONSE_CHANNEL = "ihub-host-bridge/v1";
const LEASE_HEARTBEAT_MS = 30_000;

interface PluginFrontendFrameProps {
  plugin: PluginInfo | null;
  pendingEvent: PluginFrontendEvent | null;
  onClose: () => void;
  /** uTools-compatible main-window controls remain parent-owned. The iframe
   * can request them only after the native host validates its compatibility
   * package and active visible lease. */
  onHideMainWindow?: (restorePreviousWindow: boolean) => void | Promise<void>;
  onSetExpendHeight?: (height: number) => void | Promise<void>;
  onShowMainWindow?: () => void | Promise<void>;
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
  /** A trusted launcher-host action. The iframe never receives this callback
   * and cannot choose a native window label or URL. */
  onDetach?: () => void | Promise<void>;
  /** Changes only host chrome/close wording for the native detached host. */
  placement?: "launcher" | "detached";
  /** Browser QA renders the trusted host chrome and explicit security status
   * without requesting a loopback lease or mounting a plugin iframe. */
  browserPreviewStatus?: string;
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

interface PendingCursorColorRequest {
  id: string;
  pluginId: string;
  leaseId: string;
  reply: (payload: Record<string, unknown>) => void;
}

interface PendingScreenCaptureRequest {
  id: string;
  pluginId: string;
  leaseId: string;
  reply: (payload: Record<string, unknown>) => void;
}

interface NativeScreenshot {
  dataUrl: string;
  name: string;
  mimeType: string;
  width: number;
  height: number;
}

const MAX_PLUGIN_SCREEN_CAPTURE_PNG_BYTES = 16 * 1024 * 1024;

function captureBlobDataUrl(blob: Blob): Promise<string> {
  if (blob.type !== "image/png" || blob.size < 1 || blob.size > MAX_PLUGIN_SCREEN_CAPTURE_PNG_BYTES) {
    return Promise.reject(new Error("插件截图选区必须是不超过 16 MiB 的 PNG。"));
  }
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error("无法读取插件截图选区。"));
    reader.onload = () => {
      if (typeof reader.result === "string" && reader.result.startsWith("data:image/png;base64,")) {
        resolve(reader.result);
      } else {
        reject(new Error("无法生成有效的插件截图 PNG。"));
      }
    };
    reader.readAsDataURL(blob);
  });
}

interface CursorColorApproval {
  approvalId: string;
}

interface PluginSubInputFieldProps {
  inputRef: RefObject<HTMLInputElement | null>;
  onChange: (value: string) => void;
  pluginName: string;
  placeholder: string;
  value: string;
}

interface PluginFrontendIframeProps {
  allowDisplayCapture: boolean;
  allowMicrophone: boolean;
  ariaHidden?: boolean;
  className?: string;
  frameRef: RefObject<HTMLIFrameElement | null>;
  onError: () => void;
  onLoad: () => void;
  purpose: "runtime" | "surface";
  sourceUrl: string;
  tabIndex?: number;
  title: string;
}

/**
 * The loopback plugin origin stays distinct from the trusted Tauri host. Keep
 * enough capability for the verified plugin bundle to execute while refusing
 * top navigation, popups, downloads, forms, and modal browser surfaces.
 */
export function PluginFrontendIframe({
  allowDisplayCapture,
  allowMicrophone,
  ariaHidden,
  className,
  frameRef,
  onError,
  onLoad,
  purpose,
  sourceUrl,
  tabIndex,
  title,
}: PluginFrontendIframeProps) {
  const delegatedMediaFeatures = purpose === "surface"
    ? [
        allowDisplayCapture ? "display-capture" : null,
        allowMicrophone ? "microphone" : null,
      ].filter((feature): feature is string => feature !== null)
    : [];

  return (
    <iframe
      allow={delegatedMediaFeatures.length > 0
        ? delegatedMediaFeatures.join("; ")
        : undefined}
      aria-hidden={ariaHidden}
      className={className}
      onError={onError}
      onLoad={onLoad}
      ref={frameRef}
      referrerPolicy="no-referrer"
      sandbox="allow-scripts allow-same-origin"
      src={sourceUrl}
      tabIndex={tabIndex}
      title={title}
    />
  );
}

export function PluginSubInputField({
  inputRef,
  onChange,
  pluginName,
  placeholder,
  value,
}: PluginSubInputFieldProps) {
  return (
    <label className="plugin-frame__sub-input">
      <Search aria-hidden="true" size={15} strokeWidth={2} />
      <input
        aria-label={`${pluginName} 子输入框`}
        autoComplete="off"
        maxLength={PLUGIN_SUB_INPUT_MAX_VALUE_LENGTH}
        onChange={(event) => onChange(event.currentTarget.value)}
        placeholder={placeholder}
        ref={inputRef}
        spellCheck={false}
        type="text"
        value={value}
      />
    </label>
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
  onHideMainWindow,
  onSetExpendHeight,
  onShowMainWindow,
  onPendingEventHandled,
  onToast,
  mode = "surface",
  onSurfaceReady,
  onSurfaceUnavailable,
  onRuntimeReady,
  onRuntimeDisposed,
  onSearchProviderRegistered,
  onSearchProviderUnregistered,
  onDetach,
  placement = "launcher",
  browserPreviewStatus,
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
  const [pendingScreenCaptureRequest, setPendingScreenCaptureRequest] = useState<PendingScreenCaptureRequest | null>(null);
  const [screenCaptureSource, setScreenCaptureSource] = useState<RegionCaptureSource | null>(null);
  const [screenCaptureError, setScreenCaptureError] = useState<string | null>(null);
  const [subInput, setSubInput] = useState<PluginSubInputHostState | null>(null);
  const [approvingCursorColor, setApprovingCursorColor] = useState(false);
  const [capturingPluginScreen, setCapturingPluginScreen] = useState(false);
  const [detaching, setDetaching] = useState(false);
  const readyPluginIdRef = useRef<string | null>(null);
  const postEventToFrameRef = useRef<(name: string, payload: unknown) => boolean>(() => false);
  const pendingCursorColorRequestRef = useRef<PendingCursorColorRequest | null>(null);
  const pendingScreenCaptureRequestRef = useRef<PendingScreenCaptureRequest | null>(null);
  const subInputRef = useRef<PluginSubInputHostState | null>(null);
  const subInputElementRef = useRef<HTMLInputElement>(null);
  const announcedSurfaceReadyLeaseRef = useRef<string | null>(null);
  const pluginId = plugin?.id ?? null;
  const pluginSourceKey = frontendSourceKey(plugin);
  const runtimeOnly = mode === "runtime";
  const sourceIsCurrent = source?.pluginId === pluginId;
  const sourceOrigin = sourceIsCurrent ? source?.origin ?? null : null;
  const sourceLeaseId = sourceIsCurrent ? source?.leaseId ?? null : null;
  const bridgeIsReady = sourceIsCurrent && bridgeReadyLeaseId === sourceLeaseId;
  const detachedHost = placement === "detached";
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

  useEffect(() => {
    if (!bridgeIsReady || !pluginId || runtimeOnly) {
      return;
    }
    postEventToFrame(
      `ihub://plugin/${pluginId}/event/utools.windowType`,
      { windowType: detachedHost ? "detach" : "main" },
    );
  }, [bridgeIsReady, detachedHost, pluginId, postEventToFrame, runtimeOnly]);

  const detachPluginSurface = useCallback(() => {
    if (!onDetach || !pluginId || runtimeOnly || detachedHost || detaching) {
      return;
    }
    setDetaching(true);
    void (async () => {
      try {
        await onDetach();
      } catch (reason) {
        onToast(
          reason instanceof Error ? reason.message : "无法创建插件分离窗口。",
        );
      } finally {
        setDetaching(false);
      }
    })();
  }, [detachedHost, detaching, onDetach, onToast, pluginId, runtimeOnly]);

  useEffect(() => {
    if (!onDetach || !pluginId || runtimeOnly || detachedHost) {
      return;
    }
    const onKeyDown = (event: KeyboardEvent) => {
      if (!shouldDetachPluginSurface(event, true)) {
        return;
      }
      event.preventDefault();
      detachPluginSurface();
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [detachPluginSurface, detachedHost, onDetach, pluginId, runtimeOnly]);

  const updateSubInput = useCallback((next: PluginSubInputHostState | null) => {
    subInputRef.current = next;
    setSubInput(next);
  }, []);

  useEffect(() => {
    if (!subInput || subInput.focusVersion === 0) {
      return;
    }
    subInputElementRef.current?.focus({ preventScroll: true });
  }, [subInput?.focusVersion]);

  useEffect(() => {
    if (!subInput || subInput.selectionVersion === 0) {
      return;
    }
    subInputElementRef.current?.select();
  }, [subInput?.selectionVersion]);

  const handleSubInputChange = useCallback((value: string) => {
    const current = subInputRef.current;
    if (!current || !pluginId) {
      return;
    }
    const boundedValue = value.slice(0, PLUGIN_SUB_INPUT_MAX_VALUE_LENGTH);
    updateSubInput({ ...current, value: boundedValue });
    postEventToFrameRef.current(
      `ihub://plugin/${pluginId}/event/subInput.change`,
      { text: boundedValue },
    );
  }, [pluginId, updateSubInput]);

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

  const clearPendingScreenCaptureRequest = useCallback((request: PendingScreenCaptureRequest) => {
    if (pendingScreenCaptureRequestRef.current !== request) {
      return;
    }
    pendingScreenCaptureRequestRef.current = null;
    setPendingScreenCaptureRequest(null);
    setScreenCaptureSource(null);
    setScreenCaptureError(null);
    setCapturingPluginScreen(false);
  }, []);

  const cancelPendingScreenCaptureRequest = useCallback(() => {
    const request = pendingScreenCaptureRequestRef.current;
    if (!request || capturingPluginScreen) {
      return;
    }
    request.reply({
      channel: RESPONSE_CHANNEL,
      type: "response",
      id: request.id,
      ok: false,
      error: "Screen capture was cancelled in the iHub host.",
    });
    clearPendingScreenCaptureRequest(request);
  }, [capturingPluginScreen, clearPendingScreenCaptureRequest]);

  const approvePendingScreenCaptureRequest = useCallback(() => {
    const request = pendingScreenCaptureRequestRef.current;
    if (!request || capturingPluginScreen || screenCaptureSource) {
      return;
    }
    setCapturingPluginScreen(true);
    setScreenCaptureError(null);
    void (async () => {
      try {
        const screenshot = await command<NativeScreenshot>("capture_plugin_screen_screenshot", {
          pluginId: request.pluginId,
          leaseId: request.leaseId,
        });
        if (pendingScreenCaptureRequestRef.current !== request) {
          return;
        }
        if (screenshot.mimeType !== "image/png" || !screenshot.dataUrl.startsWith("data:image/png;base64,")) {
          throw new Error("宿主没有返回有效的 PNG 截图。");
        }
        validateRegionCaptureSize(screenshot);
        setScreenCaptureSource({
          width: screenshot.width,
          height: screenshot.height,
          name: screenshot.name,
          url: screenshot.dataUrl,
        });
      } catch (reason) {
        if (pendingScreenCaptureRequestRef.current !== request) {
          return;
        }
        request.reply({
          channel: RESPONSE_CHANNEL,
          type: "response",
          id: request.id,
          ok: false,
          error: reason instanceof Error ? reason.message : "iHub could not capture the screen.",
        });
        clearPendingScreenCaptureRequest(request);
      } finally {
        if (pendingScreenCaptureRequestRef.current === request) {
          setCapturingPluginScreen(false);
        }
      }
    })();
  }, [capturingPluginScreen, clearPendingScreenCaptureRequest, screenCaptureSource]);

  const exportPendingScreenCapture = useCallback(async (capture: CroppedCapture) => {
    const request = pendingScreenCaptureRequestRef.current;
    if (!request) {
      throw new Error("插件截图请求已经失效。");
    }
    const dataUrl = await captureBlobDataUrl(capture.blob);
    if (pendingScreenCaptureRequestRef.current !== request) {
      throw new Error("插件截图请求已经失效。");
    }
    request.reply({
      channel: RESPONSE_CHANNEL,
      type: "response",
      id: request.id,
      ok: true,
      result: dataUrl,
    });
    clearPendingScreenCaptureRequest(request);
  }, [clearPendingScreenCaptureRequest]);

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
    const request = pendingScreenCaptureRequestRef.current;
    if (!request || request.leaseId === sourceLeaseId) {
      return;
    }
    request.reply({
      channel: RESPONSE_CHANNEL,
      type: "response",
      id: request.id,
      ok: false,
      error: "The plugin surface changed before screen capture was completed.",
    });
    clearPendingScreenCaptureRequest(request);
  }, [clearPendingScreenCaptureRequest, sourceLeaseId]);

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
    updateSubInput(null);
    setReadyPluginId(null);
    setCommandEventSubscriptionLeaseId(null);
    announcedSurfaceReadyLeaseRef.current = null;
    previousPluginSource.current = pluginId && pluginSourceKey
      ? { pluginId, key: pluginSourceKey }
      : null;
  }, [onRuntimeDisposed, pluginId, pluginSourceKey, updateSubInput]);

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

    if (!pluginId || !pluginSourceKey || browserPreviewStatus) {
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
  }, [browserPreviewStatus, leaseRetry, pluginId, pluginSourceKey, runtimeOnly]);

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
    const inFlight = new PluginBridgeInFlightGate();

    const onMessage = (event: MessageEvent<unknown>) => {
      const sourceWindow = frame.current?.contentWindow;
      if (
        !sourceWindow
        || event.source !== sourceWindow
        || event.origin !== sourceOrigin
      ) {
        return;
      }

      const validation = validatePluginBridgeCall(event.data, pluginId);
      if (!validation.ok) {
        if (validation.responseId) {
          try {
            sourceWindow.postMessage({
              channel: RESPONSE_CHANNEL,
              type: "response",
              id: validation.responseId,
              ok: false,
              error: validation.error,
            }, sourceOrigin);
          } catch {
            // The source navigated while its rejected call was handled.
          }
        }
        return;
      }

      const bridgeCall = validation.call;
      const admission = inFlight.begin(
        bridgeCall.id,
        isLargePluginBridgeMethod(bridgeCall.request.method),
      );
      if (admission !== "accepted") {
        try {
          sourceWindow.postMessage({
            channel: RESPONSE_CHANNEL,
            type: "response",
            id: bridgeCall.id,
            ok: false,
            error: admission === "duplicate"
              ? "A plugin Bridge request with this ID is already running."
              : "The plugin Bridge has reached its bounded in-flight request limit.",
          }, sourceOrigin);
        } catch {
          // The source navigated while its rejected call was handled.
        }
        return;
      }

      let replied = false;
      const reply = (payload: Record<string, unknown>) => {
        if (replied) {
          return;
        }
        replied = true;
        try {
          if (bridgeActive) {
            // Capture the WindowProxy that sent this request. A delayed
            // response must never be redirected to a replacement iframe.
            sourceWindow.postMessage(payload, sourceOrigin);
          }
        } finally {
          inFlight.finish(bridgeCall.id);
        }
      };

      const subInputCall = resolvePluginSubInputBridgeCall(
        subInputRef.current,
        bridgeCall.request.method,
        bridgeCall.request.params,
        runtimeOnly,
      );
      if (subInputCall.handled) {
        if (!subInputCall.ok) {
          reply({
            channel: RESPONSE_CHANNEL,
            type: "response",
            id: bridgeCall.id,
            ok: false,
            error: subInputCall.error,
          });
          return;
        }

        updateSubInput(subInputCall.state);
        if (subInputCall.focusPluginFrame) {
          window.requestAnimationFrame(() => frame.current?.focus());
        }
        if (subInputCall.emitText !== undefined) {
          postEventToFrameRef.current(
            `ihub://plugin/${pluginId}/event/subInput.change`,
            { text: subInputCall.emitText },
          );
        }
        reply({
          channel: RESPONSE_CHANNEL,
          type: "response",
          id: bridgeCall.id,
          ok: true,
          result: subInputCall.result,
        });
        return;
      }

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
        if (pendingCursorColorRequestRef.current || pendingScreenCaptureRequestRef.current) {
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
      if (bridgeCall.request.method === "compatibility.utools.screen.capture") {
        if (runtimeOnly) {
          reply({
            channel: RESPONSE_CHANNEL,
            type: "response",
            id: bridgeCall.id,
            ok: false,
            error: "Screen capture is unavailable from a hidden plugin runtime.",
          });
          return;
        }
        if (pendingScreenCaptureRequestRef.current || pendingCursorColorRequestRef.current) {
          reply({
            channel: RESPONSE_CHANNEL,
            type: "response",
            id: bridgeCall.id,
            ok: false,
            error: "Another native screen interaction is already waiting in iHub.",
          });
          return;
        }
        const pendingRequest: PendingScreenCaptureRequest = {
          id: bridgeCall.id,
          pluginId,
          leaseId: sourceLeaseId,
          reply,
        };
        // Keep the full display frame in the trusted parent. Only the user's
        // cropped PNG is posted back to the remote plugin document.
        pendingScreenCaptureRequestRef.current = pendingRequest;
        setPendingScreenCaptureRequest(pendingRequest);
        setScreenCaptureError(null);
        return;
      }
      const utoolsWindowMethod = bridgeCall.request.method.startsWith("compatibility.utools.window.")
        ? bridgeCall.request.method
        : null;
      const utoolsInputMethod = bridgeCall.request.method.startsWith("compatibility.utools.input.")
        ? bridgeCall.request.method
        : null;
      if (
        (utoolsWindowMethod || utoolsInputMethod)
        && (
          (runtimeOnly && !(utoolsInputMethod && bridgeCall.request.interactionId))
          || (
            utoolsInputMethod !== null
            && !onHideMainWindow
            && !(runtimeOnly && bridgeCall.request.interactionId)
          )
          || (utoolsWindowMethod === "compatibility.utools.window.hideMain" && !onHideMainWindow)
          || (utoolsWindowMethod === "compatibility.utools.window.setHeight" && !onSetExpendHeight)
          || (utoolsWindowMethod === "compatibility.utools.window.showMain" && !onShowMainWindow)
        )
      ) {
        reply({
          channel: RESPONSE_CHANNEL,
          type: "response",
          id: bridgeCall.id,
          ok: false,
          error: runtimeOnly
            ? "uTools window controls are unavailable from a hidden plugin runtime."
            : "This plugin host cannot control the iHub main window.",
        });
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
        interactionId: bridgeCall.request.interactionId,
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
          if (utoolsWindowMethod || utoolsInputMethod) {
            window.setTimeout(() => {
              if (utoolsInputMethod) {
                void Promise.resolve(onHideMainWindow?.(true)).catch((error) => {
                  onToast(error instanceof Error ? error.message : "iHub 主窗口未能在输入前隐藏。");
                });
              } else if (utoolsWindowMethod === "compatibility.utools.window.hideMain") {
                const params = bridgeCall.request.params as { isRestorePreWindow?: boolean } | undefined;
                void Promise.resolve(onHideMainWindow?.(params?.isRestorePreWindow ?? true)).catch((error) => {
                  onToast(error instanceof Error ? error.message : "iHub 主窗口未能隐藏。");
                });
              } else if (utoolsWindowMethod === "compatibility.utools.window.showMain") {
                void Promise.resolve(onShowMainWindow?.()).catch((error) => {
                  onToast(error instanceof Error ? error.message : "iHub 主窗口未能显示。");
                });
              } else if (utoolsWindowMethod === "compatibility.utools.window.setHeight") {
                const params = bridgeCall.request.params as { height?: number } | undefined;
                if (typeof params?.height === "number") {
                  void Promise.resolve(onSetExpendHeight?.(params.height)).catch((error) => {
                    onToast(error instanceof Error ? error.message : "iHub 插件窗口未能调整高度。");
                  });
                }
              } else if (utoolsWindowMethod === "compatibility.utools.window.outPlugin") {
                onClose();
              }
            }, 0);
          }
          if (bridgeCall.request.method === "lifecycle.ready") {
            // Runtime.activate() only makes this call after its handlers have
            // registered. Delay host-delivered work until that handshake is
            // accepted, otherwise an iframe can miss its first invocation.
            setReadyPluginId(pluginId);
          }
          if (bridgeCall.request.method === "lifecycle.dispose") {
            setReadyPluginId((current) => (current === pluginId ? null : current));
            updateSubInput(null);
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
      inFlight.clear();
      window.removeEventListener("message", onMessage);
    };
  }, [
    onClose,
    onHideMainWindow,
    onSetExpendHeight,
    onRuntimeDisposed,
    onSearchProviderRegistered,
    onSearchProviderUnregistered,
    onShowMainWindow,
    onToast,
    pluginId,
    runtimeOnly,
    sourceLeaseId,
    sourceOrigin,
    updateSubInput,
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
      enqueueBoundedPluginHostEvent(queuedHostEvents.current, { name, payload });
    };
    const subscribe = async (
      kind: "command"
        | "search"
        | "event/search.select"
        | "event/utools.dbPull"
        | "event/utools.tool.invoke"
        | "event/utools.tool.cancel",
    ) => {
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

    const subscribeBrowserEvent = async (
      nativeName: "ihub://utools-browser/parent-message" | "ihub://utools-browser/ready",
      bridgeKind: "ipc" | "ready",
    ) => {
      try {
        const stop = await listen<unknown>(nativeName, (event) => {
          if (!event.payload || typeof event.payload !== "object" || Array.isArray(event.payload)) {
            return;
          }
          forward(
            `ihub://plugin/${pluginId}/event/utools.browser.${bridgeKind}`,
            event.payload,
          );
        });
        if (disposed) stop(); else unlisten.push(stop);
      } catch {
        // BrowserWindow IPC remains closed if the narrow native channel is
        // unavailable; it is never widened to a global broadcast.
      }
    };

    void Promise.all([
      subscribe("command"),
      subscribe("search"),
      subscribe("event/search.select"),
      subscribe("event/utools.dbPull"),
      subscribe("event/utools.tool.invoke"),
      subscribe("event/utools.tool.cancel"),
      subscribeBrowserEvent("ihub://utools-browser/parent-message", "ipc"),
      subscribeBrowserEvent("ihub://utools-browser/ready", "ready"),
    ]);
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
    for (const [index, event] of events.entries()) {
      if (!postEventToFrame(event.name, event.payload)) {
        restoreFailedPluginHostEventTail(queuedHostEvents.current, events, index);
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
    rememberBoundedPluginEventId(dispatchedPendingEvents.current, pendingEvent.id);
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

  const artworkSrc = safePluginArtworkSrc(plugin?.iconSrc);

  if (runtimeOnly) {
    return source && bridgeIsReady ? (
      <PluginFrontendIframe
        allowDisplayCapture={source.allowsDisplayCapture}
        allowMicrophone={source.allowsMicrophone}
        ariaHidden
        className="plugin-search-runtime-frame"
        frameRef={frame}
        onError={() => {
          setLoading(false);
          setError("插件前端页面无法加载。");
        }}
        onLoad={() => setLoading(false)}
        purpose="runtime"
        sourceUrl={source.url}
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
          className={`plugin-frame-overlay${detachedHost ? " is-detached" : ""}`}
          initial={{ opacity: 0 }}
          animate={{ opacity: 1 }}
          exit={{ opacity: 0 }}
          transition={{ type: "spring", stiffness: 420, damping: 36 }}
        >
          <header className="plugin-frame__header">
            <div className="plugin-frame__identity">
              <button
                aria-label={detachedHost ? "关闭插件分离窗口" : "返回 iHub 启动器"}
                className="plugin-frame__back"
                onClick={onClose}
                title={detachedHost ? "关闭窗口" : "返回 iHub"}
                type="button"
              >
                {detachedHost ? (
                  <X aria-hidden="true" size={15} strokeWidth={2.1} />
                ) : (
                  <ChevronLeft aria-hidden="true" size={16} strokeWidth={2.1} />
                )}
                <span>{detachedHost ? "关闭" : "返回"}</span>
              </button>
              <span className="plugin-frame__tag">
                <span
                  aria-hidden="true"
                  className={`plugin-frame__tag-icon${artworkSrc ? " is-artwork" : ""}`}
                >
                  <PluginArtwork
                    fallback={<Puzzle size={15} strokeWidth={1.9} />}
                    iconSrc={artworkSrc}
                  />
                </span>
                <h1>{plugin.name}</h1>
              </span>
            </div>
            {subInput ? (
              <PluginSubInputField
                inputRef={subInputElementRef}
                onChange={handleSubInputChange}
                placeholder={subInput.placeholder}
                pluginName={plugin.name}
                value={subInput.value}
              />
            ) : null}
            <div className="plugin-frame__host-actions">
              {onDetach && !detachedHost ? (
                <button
                  aria-label={`在分离窗口中打开 ${plugin.name}`}
                  aria-busy={detaching}
                  className="plugin-frame__detach"
                  disabled={detaching}
                  onClick={detachPluginSurface}
                  title="分离窗口 (Ctrl+D)"
                  type="button"
                >
                  {detaching ? (
                    <LoaderCircle aria-hidden="true" className="spin" size={13} />
                  ) : (
                    <ExternalLink aria-hidden="true" size={13} strokeWidth={1.9} />
                  )}
                  <span>{detaching ? "正在分离…" : "分离窗口"}</span>
                  <kbd>Ctrl D</kbd>
                </button>
              ) : null}
              <span
                aria-label={browserPreviewStatus
                  ? "安全状态：浏览器预览未签发插件租约"
                  : "安全状态：插件界面已隔离加载"}
                className="plugin-frame__security"
                title={browserPreviewStatus
                  ? browserPreviewStatus
                  : "插件界面在独立来源中运行，只能通过受限桥接访问宿主能力。"}
              >
                <ShieldCheck aria-hidden="true" size={14} strokeWidth={1.9} />
                <span>{browserPreviewStatus ? "安全预览" : "隔离加载"}</span>
              </span>
            </div>
          </header>

          <div className="plugin-frame__content">
            {browserPreviewStatus ? (
              <div className="plugin-frame__detached-preview" role="status">
                <span className="plugin-frame__detached-preview-icon">
                  <ShieldCheck aria-hidden="true" size={22} strokeWidth={1.8} />
                </span>
                <p>插件分离窗口 · 安全预览</p>
                <small>{browserPreviewStatus}</small>
                <ul>
                  <li>固定 800 × 600 宿主窗口，可由系统边框调整大小</li>
                  <li>真实桌面端只加载 iHub 同源 React host</li>
                  <li>插件仍位于独立 loopback iframe 与受限 Bridge 内</li>
                </ul>
              </div>
            ) : null}
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
            {pendingScreenCaptureRequest ? (
              <div
                aria-label="插件截图"
                aria-modal="true"
                className="plugin-frame__screen-capture"
                role="dialog"
              >
                {screenCaptureSource ? (
                  <RegionCaptureEditor
                    exportLabel="完成截图"
                    onCancel={cancelPendingScreenCaptureRequest}
                    onExport={exportPendingScreenCapture}
                    onStatus={setScreenCaptureError}
                    source={screenCaptureSource}
                  />
                ) : (
                  <div className="plugin-frame__screen-capture-card">
                    <span>SCREEN CAPTURE</span>
                    <h2>允许 {plugin.name} 发起截图？</h2>
                    <p>iHub 将隐藏当前窗口并读取主显示器的一帧。只有你随后框选的 PNG 区域会返回给插件，完整画面不会离开可信宿主。</p>
                    {screenCaptureError ? <small role="alert">{screenCaptureError}</small> : null}
                    <div>
                      <button disabled={capturingPluginScreen} onClick={cancelPendingScreenCaptureRequest} type="button">取消</button>
                      <button disabled={capturingPluginScreen} onClick={approvePendingScreenCaptureRequest} type="button">
                        {capturingPluginScreen ? "正在截取…" : "开始截图"}
                      </button>
                    </div>
                  </div>
                )}
                {screenCaptureSource && screenCaptureError ? <p className="plugin-frame__screen-capture-error" role="alert">{screenCaptureError}</p> : null}
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
            {!browserPreviewStatus && source && bridgeIsReady ? (
              <PluginFrontendIframe
                allowDisplayCapture={source.allowsDisplayCapture}
                allowMicrophone={source.allowsMicrophone}
                frameRef={frame}
                key={`${plugin.id}:${source.leaseId}`}
                onError={() => {
                  setLoading(false);
                  setError("插件前端页面无法加载。");
                }}
                onLoad={() => setLoading(false)}
                purpose="surface"
                sourceUrl={source.url}
                title={plugin.name + " plugin frontend"}
              />
            ) : null}
          </div>
        </motion.section>
      ) : null}
    </AnimatePresence>
  );
}
