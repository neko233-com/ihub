export const LIVE_COLOR_SAMPLE_EDGE = 9;
export const LIVE_COLOR_SAMPLE_PIXELS = LIVE_COLOR_SAMPLE_EDGE * LIVE_COLOR_SAMPLE_EDGE;

export interface LiveColorPickerSession {
  sessionId: string;
  sampleEdge: number;
  minimumIntervalMs: number;
  expiresAfterMs: number;
}

export interface LiveColorSample {
  escapePressed: boolean;
  hex: string;
  leftPressed: boolean;
  pixels: string[];
  rgb: string;
  rightPressed: boolean;
  sampleEdge: number;
  x: number;
  y: number;
}

export type LiveColorPickerPhase =
  | "idle"
  | "starting"
  | "sampling"
  | "confirmed"
  | "cancelled"
  | "error";

export interface LiveColorPickerState {
  armed: boolean;
  error: string | null;
  phase: LiveColorPickerPhase;
  sample: LiveColorSample | null;
}

export type LiveColorPickerEvent =
  | { type: "start" }
  | { type: "started" }
  | { type: "sample"; sample: LiveColorSample }
  | { type: "confirm" }
  | { type: "cancel" }
  | { type: "fail"; error: string }
  | { type: "reset" };

export const initialLiveColorPickerState: LiveColorPickerState = {
  armed: false,
  error: null,
  phase: "idle",
  sample: null,
};

const safeHex = /^#[0-9A-F]{6}$/;

export function validateLiveColorPickerSession(
  value: LiveColorPickerSession,
): LiveColorPickerSession {
  if (
    typeof value.sessionId !== "string"
    || value.sessionId.length < 1
    || value.sessionId.length > 64
    || value.sampleEdge !== LIVE_COLOR_SAMPLE_EDGE
    || !Number.isSafeInteger(value.minimumIntervalMs)
    || value.minimumIntervalMs < 50
    || value.minimumIntervalMs > 1_000
    || !Number.isSafeInteger(value.expiresAfterMs)
    || value.expiresAfterMs < 1
    || value.expiresAfterMs > 30_000
  ) {
    throw new Error("取色会话没有满足 9 × 9、限频和 30 秒边界。");
  }
  return value;
}

export function normalizeLiveColorSample(value: LiveColorSample): LiveColorSample {
  if (
    value.sampleEdge !== LIVE_COLOR_SAMPLE_EDGE
    || value.pixels.length !== LIVE_COLOR_SAMPLE_PIXELS
  ) {
    throw new Error("原生取色器没有返回固定的 9 × 9 像素。");
  }
  const pixels = value.pixels.map((pixel) => pixel.toUpperCase());
  const hex = value.hex.toUpperCase();
  if (!safeHex.test(hex) || pixels.some((pixel) => !safeHex.test(pixel))) {
    throw new Error("原生取色器返回了无效颜色。");
  }
  if (pixels[Math.floor(LIVE_COLOR_SAMPLE_PIXELS / 2)] !== hex) {
    throw new Error("原生取色器中心像素与当前颜色不一致。");
  }
  if (
    !Number.isSafeInteger(value.x)
    || !Number.isSafeInteger(value.y)
    || Math.abs(value.x) > 10_000_000
    || Math.abs(value.y) > 10_000_000
  ) {
    throw new Error("原生取色器返回了无效光标坐标。");
  }
  return {
    ...value,
    hex,
    pixels,
    rgb: rgbLabel(hex),
  };
}

/**
 * A pure state machine shared by native polling and browser QA. The picker is
 * intentionally unarmed until it observes all controls released once; this
 * prevents the click that starts sampling from immediately confirming it.
 */
export function transitionLiveColorPicker(
  state: LiveColorPickerState,
  event: LiveColorPickerEvent,
): LiveColorPickerState {
  switch (event.type) {
    case "start":
      return {
        armed: false,
        error: null,
        phase: "starting",
        sample: null,
      };
    case "started":
      return state.phase === "starting"
        ? { ...state, phase: "sampling" }
        : state;
    case "sample": {
      if (state.phase !== "sampling") {
        return state;
      }
      const sample = normalizeLiveColorSample(event.sample);
      const hasCancellation = sample.rightPressed || sample.escapePressed;
      const hasConfirmation = sample.leftPressed;
      if (!state.armed) {
        return {
          ...state,
          armed: !hasCancellation && !hasConfirmation,
          sample,
        };
      }
      if (hasCancellation) {
        return { ...state, phase: "cancelled", sample };
      }
      if (hasConfirmation) {
        return { ...state, phase: "confirmed", sample };
      }
      return { ...state, sample };
    }
    case "confirm":
      return state.phase === "sampling" && state.armed && state.sample
        ? { ...state, phase: "confirmed" }
        : state;
    case "cancel":
      return state.phase === "starting" || state.phase === "sampling"
        ? { ...state, phase: "cancelled" }
        : state;
    case "fail":
      return {
        ...state,
        error: event.error,
        phase: "error",
      };
    case "reset":
      return initialLiveColorPickerState;
  }
}

function hslToHex(hue: number, saturation: number, lightness: number): string {
  const chroma = (1 - Math.abs(2 * lightness - 1)) * saturation;
  const segment = (((hue % 360) + 360) % 360) / 60;
  const intermediate = chroma * (1 - Math.abs((segment % 2) - 1));
  const [redPart, greenPart, bluePart] = segment < 1
    ? [chroma, intermediate, 0]
    : segment < 2
      ? [intermediate, chroma, 0]
      : segment < 3
        ? [0, chroma, intermediate]
        : segment < 4
          ? [0, intermediate, chroma]
          : segment < 5
            ? [intermediate, 0, chroma]
            : [chroma, 0, intermediate];
  const match = lightness - chroma / 2;
  return `#${[redPart, greenPart, bluePart]
    .map((channel) => Math.round((channel + match) * 255).toString(16).padStart(2, "0"))
    .join("")
    .toUpperCase()}`;
}

function rgbLabel(hex: string): string {
  return `rgb(${Number.parseInt(hex.slice(1, 3), 16)}, ${Number.parseInt(hex.slice(3, 5), 16)}, ${Number.parseInt(hex.slice(5, 7), 16)})`;
}

/** Deterministic live data for the development-only browser verification path. */
export function createSimulatedLiveColorSample(step: number): LiveColorSample {
  const hue = (step * 11) % 360;
  const pixels = Array.from({ length: LIVE_COLOR_SAMPLE_PIXELS }, (_, index) => {
    const row = Math.floor(index / LIVE_COLOR_SAMPLE_EDGE);
    const column = index % LIVE_COLOR_SAMPLE_EDGE;
    return hslToHex(hue + (column - 4) * 4, 0.58, 0.5 + (row - 4) * 0.035);
  });
  const hex = pixels[Math.floor(LIVE_COLOR_SAMPLE_PIXELS / 2)];
  return {
    escapePressed: false,
    hex,
    leftPressed: false,
    pixels,
    rgb: rgbLabel(hex),
    rightPressed: false,
    sampleEdge: LIVE_COLOR_SAMPLE_EDGE,
    x: 320 + step,
    y: 180 + Math.floor(step / 2),
  };
}
