export const PLUGIN_SUB_INPUT_MAX_PLACEHOLDER_LENGTH = 160;
export const PLUGIN_SUB_INPUT_MAX_VALUE_LENGTH = 4_096;

export interface PluginSubInputHostState {
  focusVersion: number;
  placeholder: string;
  selectionVersion: number;
  value: string;
}

export type PluginSubInputBridgeAction =
  | {
      kind: "set";
      placeholder: string;
      focus: boolean;
    }
  | {
      kind: "remove";
    }
  | {
      kind: "set-value";
      value: string;
    }
  | { kind: "focus" }
  | { kind: "blur" }
  | { kind: "select" };

export type PluginSubInputBridgeParseResult =
  | { handled: false }
  | { handled: true; ok: true; action: PluginSubInputBridgeAction }
  | { handled: true; ok: false; error: string };

export type PluginSubInputBridgeResolution =
  | { handled: false }
  | {
      handled: true;
      ok: false;
      error: string;
    }
  | {
      handled: true;
      ok: true;
      result: boolean;
      state: PluginSubInputHostState | null;
      emitText?: string;
      focusPluginFrame?: boolean;
    };

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object" && !Array.isArray(value);
}

function hasOnlyKeys(params: Record<string, unknown>, allowed: readonly string[]): boolean {
  const allowedKeys = new Set(allowed);
  return Object.keys(params).every((key) => allowedKeys.has(key));
}

/**
 * Parses the renderer-owned sub-input bridge methods. These calls never reach
 * Tauri or a native worker: the visible host surface owns the input and binds
 * every mutation to the iframe lease that sent the request.
 */
export function parsePluginSubInputBridgeCall(
  method: string,
  rawParams: unknown,
): PluginSubInputBridgeParseResult {
  if (
    method !== "ui.subInput.set"
    && method !== "ui.subInput.remove"
    && method !== "ui.subInput.setValue"
    && method !== "ui.subInput.focus"
    && method !== "ui.subInput.blur"
    && method !== "ui.subInput.select"
  ) {
    return { handled: false };
  }

  const params = rawParams === undefined ? {} : rawParams;
  if (!isRecord(params)) {
    return {
      handled: true,
      ok: false,
      error: "Sub-input parameters must be a JSON object.",
    };
  }

  if (method === "ui.subInput.remove") {
    if (!hasOnlyKeys(params, [])) {
      return {
        handled: true,
        ok: false,
        error: "ui.subInput.remove does not accept parameters.",
      };
    }
    return { handled: true, ok: true, action: { kind: "remove" } };
  }

  if (
    method === "ui.subInput.focus"
    || method === "ui.subInput.blur"
    || method === "ui.subInput.select"
  ) {
    if (!hasOnlyKeys(params, [])) {
      return {
        handled: true,
        ok: false,
        error: `${method} does not accept parameters.`,
      };
    }
    return {
      handled: true,
      ok: true,
      action: { kind: method.slice("ui.subInput.".length) as "focus" | "blur" | "select" },
    };
  }

  if (method === "ui.subInput.setValue") {
    if (!hasOnlyKeys(params, ["value"]) || typeof params.value !== "string") {
      return {
        handled: true,
        ok: false,
        error: "ui.subInput.setValue requires one string value.",
      };
    }
    if (params.value.length > PLUGIN_SUB_INPUT_MAX_VALUE_LENGTH) {
      return {
        handled: true,
        ok: false,
        error: `Sub-input values are limited to ${PLUGIN_SUB_INPUT_MAX_VALUE_LENGTH} characters.`,
      };
    }
    return {
      handled: true,
      ok: true,
      action: {
        kind: "set-value",
        value: params.value,
      },
    };
  }

  if (!hasOnlyKeys(params, ["placeholder", "focus"])) {
    return {
      handled: true,
      ok: false,
      error: "ui.subInput.set accepts only placeholder and focus.",
    };
  }
  if (params.placeholder !== undefined && typeof params.placeholder !== "string") {
    return {
      handled: true,
      ok: false,
      error: "Sub-input placeholder must be a string.",
    };
  }
  if (params.focus !== undefined && typeof params.focus !== "boolean") {
    return {
      handled: true,
      ok: false,
      error: "Sub-input focus must be a boolean.",
    };
  }

  const placeholder = params.placeholder ?? "";
  if (placeholder.length > PLUGIN_SUB_INPUT_MAX_PLACEHOLDER_LENGTH) {
    return {
      handled: true,
      ok: false,
      error: `Sub-input placeholders are limited to ${PLUGIN_SUB_INPUT_MAX_PLACEHOLDER_LENGTH} characters.`,
    };
  }

  return {
    handled: true,
    ok: true,
    action: {
      kind: "set",
      placeholder,
      focus: params.focus ?? true,
    },
  };
}

/**
 * Applies the visible-surface policy and returns a side-effect-free host state
 * transition. The React host only has to commit this state, optionally emit
 * one bounded change event, and send the described bridge response.
 */
export function resolvePluginSubInputBridgeCall(
  current: PluginSubInputHostState | null,
  method: string,
  rawParams: unknown,
  runtimeOnly: boolean,
): PluginSubInputBridgeResolution {
  const parsed = parsePluginSubInputBridgeCall(method, rawParams);
  if (!parsed.handled) {
    return parsed;
  }
  if (runtimeOnly) {
    return {
      handled: true,
      ok: false,
      error: "Sub-input controls are unavailable from a hidden plugin runtime.",
    };
  }
  if (!parsed.ok) {
    return {
      handled: true,
      ok: false,
      error: parsed.error,
    };
  }

  const action = parsed.action;
  if (action.kind === "set") {
    return {
      handled: true,
      ok: true,
      result: true,
      state: {
        focusVersion: action.focus
          ? (current?.focusVersion ?? 0) + 1
          : current?.focusVersion ?? 0,
        placeholder: action.placeholder,
        selectionVersion: current?.selectionVersion ?? 0,
        value: current?.value ?? "",
      },
    };
  }
  if (action.kind === "remove") {
    return {
      handled: true,
      ok: true,
      result: true,
      state: null,
      focusPluginFrame: true,
    };
  }
  if (action.kind === "blur") {
    return {
      handled: true,
      ok: true,
      result: Boolean(current),
      state: current,
      focusPluginFrame: Boolean(current),
    };
  }
  if (action.kind === "focus" || action.kind === "select") {
    if (!current) {
      return { handled: true, ok: true, result: false, state: null };
    }
    return {
      handled: true,
      ok: true,
      result: true,
      state: {
        ...current,
        focusVersion: current.focusVersion + 1,
        selectionVersion: action.kind === "select"
          ? current.selectionVersion + 1
          : current.selectionVersion,
      },
    };
  }
  if (!current) {
    return {
      handled: true,
      ok: true,
      result: false,
      state: null,
    };
  }
  return {
    handled: true,
    ok: true,
    result: true,
    state: { ...current, value: action.value },
    emitText: action.value,
  };
}
