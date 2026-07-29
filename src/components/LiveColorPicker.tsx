import { Check, LoaderCircle, MousePointer2, Palette, X } from "lucide-react";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { command, isDesktop } from "../lib/desktop";
import {
  createSimulatedLiveColorSample,
  initialLiveColorPickerState,
  normalizeLiveColorSample,
  transitionLiveColorPicker,
  validateLiveColorPickerSession,
  type LiveColorPickerEvent,
  type LiveColorPickerSession,
  type LiveColorPickerState,
  type LiveColorSample,
} from "../lib/live-color-picker";

interface LiveColorPickerTransport {
  begin(): Promise<LiveColorPickerSession>;
  end(sessionId: string): Promise<void>;
  sample(sessionId: string): Promise<LiveColorSample>;
}

interface LiveColorPickerProps {
  onConfirm: (sample: LiveColorSample) => Promise<void> | void;
  onStatus: (message: string) => void;
}

function nativeColorPickerTransport(): LiveColorPickerTransport {
  return {
    begin: () => command<LiveColorPickerSession>("begin_cursor_color_picker"),
    end: (sessionId) => command<void>("end_cursor_color_picker", { sessionId }),
    sample: async (sessionId) => normalizeLiveColorSample(
      await command<LiveColorSample>("sample_cursor_color_neighborhood", { sessionId }),
    ),
  };
}

function simulatedColorPickerTransport(): LiveColorPickerTransport {
  let step = 0;
  let startedAt = 0;
  return {
    begin: async () => {
      startedAt = Date.now();
      step = 0;
      return {
        sessionId: "browser-development-simulation",
        sampleEdge: 9,
        minimumIntervalMs: 96,
        expiresAfterMs: 30_000,
      };
    },
    end: async () => {
      startedAt = 0;
    },
    sample: async () => {
      if (!startedAt || Date.now() - startedAt > 30_000) {
        throw new Error("模拟取色会话已在 30 秒后结束。");
      }
      return createSimulatedLiveColorSample(step++);
    },
  };
}

function pickerStatusLabel(state: LiveColorPickerState): string {
  switch (state.phase) {
    case "starting":
      return "正在申请短时取色会话…";
    case "sampling":
      return state.armed ? "移动光标实时取色" : "松开鼠标后开始取色";
    case "confirmed":
      return `已确认 ${state.sample?.hex ?? ""}`;
    case "cancelled":
      return "已取消取色";
    case "error":
      return state.error ?? "实时取色不可用";
    default:
      return "开始后显示光标周围 9 × 9 像素";
  }
}

export function LiveColorPicker({ onConfirm, onStatus }: LiveColorPickerProps) {
  const desktop = isDesktop();
  const browserSimulation = !desktop && import.meta.env.DEV;
  const transport = useMemo(
    () => desktop ? nativeColorPickerTransport() : simulatedColorPickerTransport(),
    [desktop],
  );
  const [state, setState] = useState(initialLiveColorPickerState);
  const stateRef = useRef(state);
  const sessionRef = useRef<LiveColorPickerSession | null>(null);
  const timerRef = useRef<number | null>(null);
  const startAttemptRef = useRef(0);
  const terminalHandledRef = useRef<LiveColorPickerState["phase"] | null>(null);

  const updateState = useCallback((event: LiveColorPickerEvent) => {
    const next = transitionLiveColorPicker(stateRef.current, event);
    stateRef.current = next;
    setState(next);
    return next;
  }, []);

  const clearPollingTimer = useCallback(() => {
    if (timerRef.current !== null) {
      window.clearTimeout(timerRef.current);
      timerRef.current = null;
    }
  }, []);

  const releaseSession = useCallback(() => {
    clearPollingTimer();
    const session = sessionRef.current;
    sessionRef.current = null;
    if (session) {
      void transport.end(session.sessionId).catch(() => undefined);
    }
  }, [clearPollingTimer, transport]);

  const pollSession = useCallback(async function poll(
    session: LiveColorPickerSession,
  ): Promise<void> {
    if (sessionRef.current?.sessionId !== session.sessionId) {
      return;
    }
    try {
      const sample = await transport.sample(session.sessionId);
      if (sessionRef.current?.sessionId !== session.sessionId) {
        return;
      }
      const next = updateState({ type: "sample", sample });
      if (next.phase !== "sampling") {
        return;
      }
      const delay = Math.max(84, session.minimumIntervalMs);
      timerRef.current = window.setTimeout(() => {
        timerRef.current = null;
        void poll(session);
      }, delay);
    } catch (error) {
      if (sessionRef.current?.sessionId !== session.sessionId) {
        return;
      }
      updateState({
        type: "fail",
        error: error instanceof Error ? error.message : "无法读取光标周围像素。",
      });
    }
  }, [transport, updateState]);

  const startPicker = async () => {
    if (!desktop && !browserSimulation) {
      onStatus("实时原生取色仅在 iHub 桌面应用中可用。");
      return;
    }
    if (stateRef.current.phase === "starting" || stateRef.current.phase === "sampling") {
      return;
    }

    releaseSession();
    terminalHandledRef.current = null;
    updateState({ type: "start" });
    const attempt = ++startAttemptRef.current;
    try {
      const issuedSession = await transport.begin();
      let session: LiveColorPickerSession;
      try {
        session = validateLiveColorPickerSession(issuedSession);
      } catch (error) {
        if (
          typeof issuedSession.sessionId === "string"
          && issuedSession.sessionId.length > 0
          && issuedSession.sessionId.length <= 64
        ) {
          await transport.end(issuedSession.sessionId).catch(() => undefined);
        }
        throw error;
      }
      if (startAttemptRef.current !== attempt) {
        await transport.end(session.sessionId).catch(() => undefined);
        return;
      }
      sessionRef.current = session;
      updateState({ type: "started" });
      void pollSession(session);
    } catch (error) {
      if (startAttemptRef.current !== attempt) {
        return;
      }
      updateState({
        type: "fail",
        error: error instanceof Error ? error.message : "无法开始实时取色。",
      });
    }
  };

  const cancelPicker = useCallback(() => {
    startAttemptRef.current += 1;
    updateState({ type: "cancel" });
  }, [updateState]);

  const confirmPicker = useCallback((event?: ReactPointerEvent) => {
    event?.preventDefault();
    updateState({ type: "confirm" });
  }, [updateState]);

  useEffect(() => {
    const handleEscape = (event: KeyboardEvent) => {
      if (
        event.key !== "Escape"
        || (stateRef.current.phase !== "starting" && stateRef.current.phase !== "sampling")
      ) {
        return;
      }
      event.preventDefault();
      cancelPicker();
    };
    window.addEventListener("keydown", handleEscape, { capture: true });
    return () => window.removeEventListener("keydown", handleEscape, { capture: true });
  }, [cancelPicker]);

  useEffect(() => {
    if (
      state.phase !== "confirmed"
      && state.phase !== "cancelled"
      && state.phase !== "error"
    ) {
      return;
    }
    if (terminalHandledRef.current === state.phase) {
      return;
    }
    terminalHandledRef.current = state.phase;
    releaseSession();
    if (state.phase === "confirmed" && state.sample) {
      void onConfirm(state.sample);
    } else if (state.phase === "cancelled") {
      onStatus("已取消实时取色。");
    } else if (state.error) {
      onStatus(state.error);
    }
  }, [onConfirm, onStatus, releaseSession, state]);

  useEffect(() => () => {
    startAttemptRef.current += 1;
    clearPollingTimer();
    const session = sessionRef.current;
    sessionRef.current = null;
    if (session) {
      void transport.end(session.sessionId).catch(() => undefined);
    }
  }, [clearPollingTimer, transport]);

  const active = state.phase === "starting" || state.phase === "sampling";
  const sample = state.sample;

  return (
    <section
      aria-label="实时 9 × 9 取色器"
      className="live-color-picker"
      data-phase={state.phase}
    >
      <div className="live-color-picker__toolbar">
        <button
          className="toolbox-record-action live-color-picker__start"
          disabled={active || (!desktop && !browserSimulation)}
          onClick={() => void startPicker()}
          type="button"
        >
          {state.phase === "starting" ? <LoaderCircle className="spin" size={15} /> : <Palette size={15} />}
          {active
            ? "实时取色中…"
            : browserSimulation
              ? "启动模拟取色（开发验证）"
              : "开始实时取色"}
        </button>
        {active ? (
          <button
            className="toolbox-secondary-action"
            onClick={cancelPicker}
            onPointerDown={(event) => {
              if (event.button === 0) {
                cancelPicker();
              }
            }}
            type="button"
          >
            <X size={14} />
            取消
          </button>
        ) : null}
      </div>

      {sample ? (
        <div className="live-color-picker__surface">
          <button
            aria-label={`确认颜色 ${sample.hex}`}
            className="live-color-picker__magnifier"
            onContextMenu={(event) => {
              event.preventDefault();
              cancelPicker();
            }}
            onPointerDown={(event) => {
              if (event.button === 2) {
                cancelPicker();
              } else if (event.button === 0) {
                confirmPicker(event);
              }
            }}
            style={{
              gridTemplateColumns: `repeat(${sample.sampleEdge}, 1fr)`,
            }}
            type="button"
          >
            {sample.pixels.map((pixel, index) => (
              <span
                className={index === 40 ? "is-center" : undefined}
                data-center={index === 40 ? "true" : undefined}
                key={`${index}:${pixel}`}
                style={{ backgroundColor: pixel }}
              />
            ))}
            <MousePointer2 aria-hidden="true" className="live-color-picker__cursor" size={18} />
          </button>
          <div className="live-color-picker__readout">
            <span>光标 {sample.x}, {sample.y}</span>
            <strong>{sample.hex}</strong>
            <small>{sample.rgb}</small>
            <button
              className="toolbox-secondary-action"
              disabled={!state.armed || state.phase !== "sampling"}
              onClick={() => confirmPicker()}
              type="button"
            >
              <Check size={14} />
              确认并复制
            </button>
          </div>
        </div>
      ) : null}

      <p aria-live="polite" className="live-color-picker__status">
        {pickerStatusLabel(state)}
      </p>
      <p className="toolbox-note live-color-picker__note">
        最多 15 次/秒、每次固定 9 × 9 像素、会话最长 30 秒；左键确认并复制，右键或 Esc 取消。只读像素与按键状态，不移动光标、不注入输入、不保存历史。
      </p>
    </section>
  );
}
