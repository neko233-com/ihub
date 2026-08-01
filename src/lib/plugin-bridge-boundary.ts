const PLUGIN_BRIDGE_REQUEST_CHANNEL = "ihub-plugin-bridge/v1";

export const PLUGIN_BRIDGE_MAX_ID_LENGTH = 128;
export const PLUGIN_BRIDGE_MAX_METHOD_LENGTH = 64;
export const PLUGIN_BRIDGE_MAX_JSON_BYTES = 64 * 1024;
export const PLUGIN_BRIDGE_MAX_IMAGE_PNG_BYTES = 4 * 1024 * 1024;
export const PLUGIN_BRIDGE_MAX_IMAGE_DATA_URL_CHARS = "data:image/png;base64,".length
  + Math.ceil(PLUGIN_BRIDGE_MAX_IMAGE_PNG_BYTES / 3) * 4;
// The generic walker conservatively counts three bytes for every UTF-16 code
// unit. A PNG data URL is ASCII, but retain that conservative accounting and
// enlarge the envelope for this exact, shape-checked method.
export const PLUGIN_BRIDGE_MAX_IMAGE_JSON_BYTES = 17 * 1024 * 1024;
// uTools documents are capped again by the native store at 1 MiB each and an
// 8 MiB bulk input. The iterative browser walker charges three bytes per
// UTF-16 unit, so this conservative envelope is reserved for exact DB writes.
export const PLUGIN_BRIDGE_MAX_DB_JSON_BYTES = 25 * 1024 * 1024;
export const PLUGIN_BRIDGE_MAX_DB_QUERY_JSON_BYTES = 512 * 1024;
export const PLUGIN_BRIDGE_MAX_ATTACHMENT_BYTES = 10 * 1024 * 1024;
export const PLUGIN_BRIDGE_MAX_ATTACHMENT_BASE64_CHARS = Math.ceil(
  PLUGIN_BRIDGE_MAX_ATTACHMENT_BYTES / 3,
) * 4;
export const PLUGIN_BRIDGE_MAX_ATTACHMENT_JSON_BYTES = 43 * 1024 * 1024;
export const PLUGIN_BRIDGE_MAX_JSON_DEPTH = 32;
export const PLUGIN_BRIDGE_MAX_JSON_NODES = 4_096;
export const PLUGIN_BRIDGE_MAX_DB_JSON_NODES = 65_536;
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
  "compatibility.utools.clipboard.writeImage",
  "compatibility.utools.clipboard.writeFiles",
  "compatibility.utools.db.allDocs",
  "compatibility.utools.db.bulkDocs",
  "compatibility.utools.db.get",
  "compatibility.utools.db.getAttachment",
  "compatibility.utools.db.getAttachmentType",
  "compatibility.utools.db.postAttachment",
  "compatibility.utools.db.put",
  "compatibility.utools.db.remove",
  "compatibility.utools.dbStorage.remove",
  "compatibility.utools.dbStorage.set",
  "compatibility.utools.dbStorage.snapshot",
  "compatibility.utools.features.remove",
  "compatibility.utools.features.set",
  "compatibility.utools.features.snapshot",
  "compatibility.utools.input.pasteText",
  "compatibility.utools.input.pasteImage",
  "compatibility.utools.input.pasteFiles",
  "compatibility.utools.input.typeString",
  "compatibility.utools.notification.show",
  "compatibility.utools.screen.capture",
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

function boundedJsonValue(
  value: unknown,
  maxBytes = PLUGIN_BRIDGE_MAX_JSON_BYTES,
  maxNodes = PLUGIN_BRIDGE_MAX_JSON_NODES,
): boolean {
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
        nodes > maxNodes
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

      if (bytes > maxBytes) {
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
  const isImageCopy = method === "compatibility.utools.clipboard.writeImage"
    || method === "compatibility.utools.input.pasteImage";
  const isDbWrite = method === "compatibility.utools.db.put"
    || method === "compatibility.utools.db.bulkDocs";
  const isDbAllDocs = method === "compatibility.utools.db.allDocs";
  const isAttachmentWrite = method === "compatibility.utools.db.postAttachment";
  if (method === "compatibility.utools.screen.capture") {
    const params = isPlainRecord(request.params) ? request.params : null;
    if (!params || !hasOnlyKeys(params, new Set())) {
      return {
        ok: false,
        error: "uTools screenCapture does not accept capture options.",
        responseId,
      };
    }
  }
  if (isImageCopy) {
    const params = isPlainRecord(request.params) ? request.params : null;
    const dataUrl = params?.dataUrl;
    if (
      !params
      || !hasOnlyKeys(params, new Set(["dataUrl"]))
      || typeof dataUrl !== "string"
      || !dataUrl.startsWith("data:image/png;base64,iVBORw0KGgo")
      || dataUrl.length > PLUGIN_BRIDGE_MAX_IMAGE_DATA_URL_CHARS
    ) {
      return {
        ok: false,
        error: "uTools image transfer accepts one bounded PNG data URL.",
        responseId,
      };
    }
  }
  if (isDbWrite) {
    const params = isPlainRecord(request.params) ? request.params : null;
    const expectedKey = method.endsWith(".put") ? "doc" : "docs";
    const payload = params?.[expectedKey];
    const validPayload = expectedKey === "doc"
      ? isPlainRecord(payload)
      : Array.isArray(payload)
        && payload.length >= 1
        && payload.length <= 16
        && payload.every(isPlainRecord);
    if (
      !params
      || !hasOnlyKeys(params, new Set([expectedKey]))
      || !validPayload
    ) {
      return {
        ok: false,
        error: method.endsWith(".put")
          ? "uTools db.put accepts one document object."
          : "uTools db.bulkDocs accepts 1-16 document objects.",
        responseId,
      };
    }
  }
  if (isAttachmentWrite) {
    const params = isPlainRecord(request.params) ? request.params : null;
    const id = params?.id;
    const dataBase64 = params?.dataBase64;
    const contentType = params?.contentType;
    if (
      !params
      || !hasOnlyKeys(params, new Set(["id", "dataBase64", "contentType"]))
      || typeof id !== "string"
      || id.length === 0
      || id.length > 512
      || typeof dataBase64 !== "string"
      || dataBase64.length === 0
      || dataBase64.length > PLUGIN_BRIDGE_MAX_ATTACHMENT_BASE64_CHARS
      || typeof contentType !== "string"
      || contentType.length === 0
      || contentType.length > 255
    ) {
      return {
        ok: false,
        error: "uTools postAttachment accepts one bounded ID, MIME type, and 10 MiB attachment.",
        responseId,
      };
    }
  }
  let maxJsonBytes = PLUGIN_BRIDGE_MAX_JSON_BYTES;
  if (isImageCopy) {
    maxJsonBytes = PLUGIN_BRIDGE_MAX_IMAGE_JSON_BYTES;
  } else if (isDbWrite) {
    maxJsonBytes = PLUGIN_BRIDGE_MAX_DB_JSON_BYTES;
  } else if (isAttachmentWrite) {
    maxJsonBytes = PLUGIN_BRIDGE_MAX_ATTACHMENT_JSON_BYTES;
  } else if (isDbAllDocs) {
    maxJsonBytes = PLUGIN_BRIDGE_MAX_DB_QUERY_JSON_BYTES;
  }
  const maxJsonNodes = isDbWrite
    ? PLUGIN_BRIDGE_MAX_DB_JSON_NODES
    : PLUGIN_BRIDGE_MAX_JSON_NODES;
  if (!boundedJsonValue(normalizedCall, maxJsonBytes, maxJsonNodes)) {
    return {
      ok: false,
      error: `Plugin Bridge requests are limited to ${maxJsonBytes} bytes, ${PLUGIN_BRIDGE_MAX_JSON_DEPTH} levels, and ${maxJsonNodes} values.`,
      responseId,
    };
  }

  return {
    ok: true,
    call: normalizedCall,
  };
}

export class PluginBridgeInFlightGate {
  readonly #active = new Map<string, boolean>();

  begin(id: string, large = false): "accepted" | "duplicate" | "busy" {
    if (this.#active.has(id)) {
      return "duplicate";
    }
    if (this.#active.size >= PLUGIN_BRIDGE_MAX_IN_FLIGHT) {
      return "busy";
    }
    if (large && [...this.#active.values()].some(Boolean)) {
      return "busy";
    }
    this.#active.set(id, large);
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

export function isLargePluginBridgeMethod(method: string): boolean {
  return method === "compatibility.utools.clipboard.writeImage"
    || method === "compatibility.utools.input.pasteImage"
    || method === "compatibility.utools.db.put"
    || method === "compatibility.utools.db.bulkDocs"
    || method === "compatibility.utools.db.postAttachment";
}
