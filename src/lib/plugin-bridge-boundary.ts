const PLUGIN_BRIDGE_REQUEST_CHANNEL = "ihub-plugin-bridge/v1";

export const PLUGIN_BRIDGE_MAX_ID_LENGTH = 128;
export const PLUGIN_BRIDGE_MAX_METHOD_LENGTH = 64;
export const PLUGIN_BRIDGE_MAX_JSON_BYTES = 64 * 1024;
export const PLUGIN_BRIDGE_MAX_JSON_DEPTH = 32;
export const PLUGIN_BRIDGE_MAX_JSON_NODES = 4_096;
export const PLUGIN_BRIDGE_MAX_IN_FLIGHT = 32;

const pluginHostMethods = new Set([
  "clipboard.history.snapshot",
  "clipboard.read",
  "clipboard.readText",
  "clipboard.write",
  "clipboard.writeText",
  "commands.complete",
  "commands.execute",
  "commands.register",
  "commands.unregister",
  "compatibility.utools.clipboard.writeText",
  "compatibility.utools.dbStorage.remove",
  "compatibility.utools.dbStorage.set",
  "compatibility.utools.dbStorage.snapshot",
  "compatibility.utools.features.remove",
  "compatibility.utools.features.set",
  "compatibility.utools.features.snapshot",
  "compatibility.utools.input.pasteText",
  "compatibility.utools.input.typeString",
  "compatibility.utools.notification.show",
  "compatibility.utools.shell.beep",
  "compatibility.utools.shell.openExternal",
  "compatibility.utools.window.hideMain",
  "compatibility.utools.window.outPlugin",
  "compatibility.utools.window.setHeight",
  "compatibility.utools.window.showMain",
  "cursorColor.sampleOnce",
  "developer.createProject",
  "filesystem.batchRename.apply",
  "filesystem.batchRename.preview",
  "filesystem.selectDirectory",
  "filesystem.selectFiles",
  "launcherContext.consume",
  "lifecycle.dispose",
  "lifecycle.ready",
  "log",
  "native.runCommand",
  "notifications.show",
  "screenCapture.acquireFocusLease",
  "screenCapture.releaseFocusLease",
  "search.complete",
  "search.register",
  "search.unregister",
  "settings.get",
  "settings.set",
  "shell.open",
  "shell.openExternal",
  "shell.openPath",
  "ui.subInput.remove",
  "ui.subInput.blur",
  "ui.subInput.focus",
  "ui.subInput.select",
  "ui.subInput.set",
  "ui.subInput.setValue",
  "window.manageLauncher",
]);

export interface ValidatedPluginBridgeCall {
  channel: typeof PLUGIN_BRIDGE_REQUEST_CHANNEL;
  type: "call";
  id: string;
  request: {
    pluginId: string;
    method: string;
    params?: unknown;
  };
}

export type PluginBridgeValidation =
  | { ok: true; call: ValidatedPluginBridgeCall }
  | { ok: false; error: string; responseId?: string };

function isPlainRecord(value: unknown): value is Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    return false;
  }
  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

function hasOnlyKeys(record: Record<string, unknown>, allowed: ReadonlySet<string>): boolean {
  const keys = Object.keys(record);
  return keys.every((key) => allowed.has(key))
    && Object.getOwnPropertySymbols(record).length === 0;
}

function boundedJsonValue(value: unknown): boolean {
  const stack: Array<{ value: unknown; depth: number }> = [{ value, depth: 0 }];
  const seen = new WeakSet<object>();
  let bytes = 0;
  let nodes = 0;

  const addString = (text: string) => {
    // Three bytes per UTF-16 code unit is a conservative UTF-8 upper bound
    // (surrogate pairs use four bytes total). Include JSON quotes/escape room.
    bytes += text.length * 3 + 2;
  };

  try {
    while (stack.length > 0) {
      const current = stack.pop()!;
      nodes += 1;
      if (
        nodes > PLUGIN_BRIDGE_MAX_JSON_NODES
        || current.depth > PLUGIN_BRIDGE_MAX_JSON_DEPTH
      ) {
        return false;
      }

      if (current.value === null) {
        bytes += 4;
      } else if (typeof current.value === "string") {
        addString(current.value);
      } else if (typeof current.value === "boolean") {
        bytes += 5;
      } else if (typeof current.value === "number") {
        if (!Number.isFinite(current.value)) {
          return false;
        }
        bytes += 24;
      } else if (Array.isArray(current.value)) {
        if (seen.has(current.value)) {
          return false;
        }
        seen.add(current.value);
        bytes += current.value.length + 2;
        for (let index = current.value.length - 1; index >= 0; index -= 1) {
          stack.push({ value: current.value[index], depth: current.depth + 1 });
        }
      } else if (isPlainRecord(current.value)) {
        if (seen.has(current.value)) {
          return false;
        }
        seen.add(current.value);
        const keys = Object.keys(current.value);
        bytes += keys.length + 2;
        for (let index = keys.length - 1; index >= 0; index -= 1) {
          const key = keys[index]!;
          addString(key);
          stack.push({ value: current.value[key], depth: current.depth + 1 });
        }
      } else {
        return false;
      }

      if (bytes > PLUGIN_BRIDGE_MAX_JSON_BYTES) {
        return false;
      }
    }
  } catch {
    return false;
  }
  return true;
}

function safeResponseId(value: unknown): string | undefined {
  if (
    typeof value !== "string"
    || value.length === 0
    || value.length > PLUGIN_BRIDGE_MAX_ID_LENGTH
    || value.trim() !== value
    || [...value].some((character) => character < " " || character === "\u007f")
  ) {
    return undefined;
  }
  return value;
}

/**
 * Rejects an iframe request before it can allocate a Tauri IPC invocation.
 * postMessage has already produced a structured clone, so this validator is
 * deliberately iterative and bounded as well: it never stringifies or
 * recursively walks an attacker-controlled graph.
 */
export function validatePluginBridgeCall(
  value: unknown,
  expectedPluginId?: string,
): PluginBridgeValidation {
  const record = isPlainRecord(value) ? value : null;
  const responseId = safeResponseId(record?.id);
  if (
    !record
    || record.channel !== PLUGIN_BRIDGE_REQUEST_CHANNEL
    || record.type !== "call"
    || !responseId
    || !hasOnlyKeys(record, new Set(["channel", "type", "id", "request"]))
  ) {
    return {
      ok: false,
      error: "The plugin Bridge request envelope is invalid.",
      responseId,
    };
  }

  const request = isPlainRecord(record.request) ? record.request : null;
  const method = request?.method;
  const requestPluginId = request?.pluginId;
  if (
    !request
    || !hasOnlyKeys(request, new Set(["pluginId", "method", "params"]))
    || typeof requestPluginId !== "string"
    || !/^[A-Za-z0-9._-]{2,96}$/.test(requestPluginId)
    || (expectedPluginId !== undefined && requestPluginId !== expectedPluginId)
    || typeof method !== "string"
    || method.length === 0
    || method.length > PLUGIN_BRIDGE_MAX_METHOD_LENGTH
    || !pluginHostMethods.has(method)
  ) {
    return {
      ok: false,
      error: "The plugin Bridge method is unsupported or malformed.",
      responseId,
    };
  }

  const normalizedRequest = request.params === undefined
    ? { pluginId: requestPluginId, method }
    : { pluginId: requestPluginId, method, params: request.params };
  const normalizedCall = {
    channel: PLUGIN_BRIDGE_REQUEST_CHANNEL as typeof PLUGIN_BRIDGE_REQUEST_CHANNEL,
    type: "call" as const,
    id: responseId,
    request: normalizedRequest,
  };
  if (!boundedJsonValue(normalizedCall)) {
    return {
      ok: false,
      error: `Plugin Bridge requests are limited to ${PLUGIN_BRIDGE_MAX_JSON_BYTES} bytes, ${PLUGIN_BRIDGE_MAX_JSON_DEPTH} levels, and ${PLUGIN_BRIDGE_MAX_JSON_NODES} values.`,
      responseId,
    };
  }

  return {
    ok: true,
    call: normalizedCall,
  };
}

export class PluginBridgeInFlightGate {
  readonly #active = new Set<string>();

  begin(id: string): "accepted" | "duplicate" | "busy" {
    if (this.#active.has(id)) {
      return "duplicate";
    }
    if (this.#active.size >= PLUGIN_BRIDGE_MAX_IN_FLIGHT) {
      return "busy";
    }
    this.#active.add(id);
    return "accepted";
  }

  finish(id: string): void {
    this.#active.delete(id);
  }

  clear(): void {
    this.#active.clear();
  }

  get size(): number {
    return this.#active.size;
  }
}
