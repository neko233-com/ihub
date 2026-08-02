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
export const PLUGIN_BRIDGE_MAX_CRYPTO_STORAGE_JSON_BYTES = 256 * 1024;
export const PLUGIN_BRIDGE_MAX_BROWSER_JSON_BYTES = 800 * 1024;
export const PLUGIN_BRIDGE_MAX_UBROWSER_JSON_BYTES = 4 * 1024 * 1024;
export const PLUGIN_BRIDGE_MAX_UBROWSER_JSON_NODES = 32_768;
export const PLUGIN_BRIDGE_MAX_UTOOLS_TOOL_JSON_BYTES = 4 * 1024 * 1024;
export const PLUGIN_BRIDGE_MAX_UTOOLS_TOOL_JSON_NODES = 16_384;
export const PLUGIN_BRIDGE_MAX_UTOOLS_AI_JSON_BYTES = 2 * 1024 * 1024;
export const PLUGIN_BRIDGE_MAX_UTOOLS_AI_JSON_NODES = 32_768;
export const PLUGIN_BRIDGE_MAX_UTOOLS_SHARP_JSON_BYTES = 70 * 1024 * 1024;
export const PLUGIN_BRIDGE_MAX_UTOOLS_SHARP_JSON_NODES = 16_384;
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
  "compatibility.utools.browser.closeSelf",
  "compatibility.utools.browser.control",
  "compatibility.utools.browser.create",
  "compatibility.utools.browser.executeJavaScript",
  "compatibility.utools.browser.executeResult",
  "compatibility.utools.browser.send",
  "compatibility.utools.browser.sendToParent",
  "compatibility.utools.ubrowser.run",
  "compatibility.utools.ubrowser.setProxy",
  "compatibility.utools.ubrowser.clearCache",
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
  "compatibility.utools.dbCryptoStorage.remove",
  "compatibility.utools.dbCryptoStorage.set",
  "compatibility.utools.dbCryptoStorage.snapshot",
  "compatibility.utools.features.remove",
  "compatibility.utools.features.set",
  "compatibility.utools.features.snapshot",
  "compatibility.utools.input.pasteText",
  "compatibility.utools.input.pasteImage",
  "compatibility.utools.input.pasteFiles",
  "compatibility.utools.input.typeString",
  "compatibility.utools.mainPush.selectComplete",
  "compatibility.utools.tools.register",
  "compatibility.utools.tools.complete",
  "compatibility.utools.tools.progress",
  "compatibility.utools.ai.models",
  "compatibility.utools.ai.start",
  "compatibility.utools.ai.abort",
  "compatibility.utools.ai.toolComplete",
  "compatibility.utools.ffmpeg.start",
  "compatibility.utools.ffmpeg.kill",
  "compatibility.utools.ffmpeg.quit",
  "compatibility.utools.sharp.execute",
  "compatibility.utools.notification.show",
  "compatibility.utools.screen.capture",
  "compatibility.utools.shell.beep",
  "compatibility.utools.shell.openExternal",
  "compatibility.utools.shell.openPath",
  "compatibility.utools.shell.showItemInFolder",
  "compatibility.utools.shell.trashItem",
  "compatibility.utools.simulate.keyboardTap",
  "compatibility.utools.simulate.mouseClick",
  "compatibility.utools.simulate.mouseDoubleClick",
  "compatibility.utools.simulate.mouseMove",
  "compatibility.utools.simulate.mouseRightClick",
  "compatibility.utools.system.readCurrentBrowserUrl",
  "compatibility.utools.system.readCurrentFolderPath",
  "compatibility.utools.window.hideMain",
  "compatibility.utools.window.outPlugin",
  "compatibility.utools.window.redirect",
  "compatibility.utools.window.setHeight",
  "compatibility.utools.window.showMain",
  "compatibility.utools.window.startDrag",
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
    interactionId?: string;
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
    || !hasOnlyKeys(request, new Set(["pluginId", "method", "params", "interactionId"]))
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

  if (request.interactionId !== undefined && !safeResponseId(request.interactionId)) {
    return {
      ok: false,
      error: "The plugin Bridge interaction ID is invalid.",
      responseId,
    };
  }

  const normalizedRequest = {
    pluginId: requestPluginId,
    method,
    ...(request.params === undefined ? {} : { params: request.params }),
    ...(request.interactionId === undefined
      ? {}
      : { interactionId: request.interactionId as string }),
  };
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
  const isCryptoStorageWrite = method === "compatibility.utools.dbCryptoStorage.set";
  const isUtoolsFileList = method === "compatibility.utools.clipboard.writeFiles"
    || method === "compatibility.utools.window.startDrag";
  const isUtoolsRedirect = method === "compatibility.utools.window.redirect";
  const isUtoolsSimulation = method.startsWith("compatibility.utools.simulate.");
  const isUtoolsBrowser = method.startsWith("compatibility.utools.browser.");
  const isUtoolsUBrowser = method === "compatibility.utools.ubrowser.run";
  const isUtoolsTool = method.startsWith("compatibility.utools.tools.");
  const isUtoolsAi = method.startsWith("compatibility.utools.ai.");
  const isUtoolsFfmpeg = method.startsWith("compatibility.utools.ffmpeg.");
  const isUtoolsSharp = method === "compatibility.utools.sharp.execute";
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
    const path = params?.path;
    if (
      !params
      || (
        hasOnlyKeys(params, new Set(["dataUrl"]))
          ? typeof dataUrl !== "string"
            || !dataUrl.startsWith("data:image/png;base64,iVBORw0KGgo")
            || dataUrl.length > PLUGIN_BRIDGE_MAX_IMAGE_DATA_URL_CHARS
          : !hasOnlyKeys(params, new Set(["path"]))
            || typeof path !== "string"
            || path.length === 0
            || [...path].length > 1_024
            || new TextEncoder().encode(path).byteLength > 8 * 1_024
            || [...path].some((character) => character < " " || character === "\u007f")
      )
    ) {
      return {
        ok: false,
        error: "uTools image transfer accepts one bounded PNG data URL or picker-returned path.",
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
  if (isCryptoStorageWrite) {
    const params = isPlainRecord(request.params) ? request.params : null;
    const key = params?.key;
    if (
      !params
      || !hasOnlyKeys(params, new Set(["key", "value"]))
      || typeof key !== "string"
      || new TextEncoder().encode(key).byteLength > 48
    ) {
      return {
        ok: false,
        error: "uTools dbCryptoStorage.set accepts one bounded string key and JSON value.",
        responseId,
      };
    }
  }
  if (isUtoolsFileList) {
    const params = isPlainRecord(request.params) ? request.params : null;
    const paths = params?.paths;
    if (
      !params
      || !hasOnlyKeys(params, new Set(["paths"]))
      || !Array.isArray(paths)
      || paths.length < 1
      || paths.length > 16
      || paths.some((path) => (
        typeof path !== "string"
        || path.length === 0
        || [...path].length > 1_024
        || [...path].some((character) => character < " " || character === "\u007f")
      ))
      || new TextEncoder().encode(paths.join("")).byteLength > 8 * 1_024
      || new Set(paths).size !== paths.length
    ) {
      return {
        ok: false,
        error: "uTools file transfer requires 1-16 unique bounded path strings.",
        responseId,
      };
    }
  }
  if (isUtoolsRedirect) {
    const params = isPlainRecord(request.params) ? request.params : null;
    const label = params?.label;
    const action = isPlainRecord(params?.action) ? params.action : null;
    const labelParts = typeof label === "string"
      ? [label]
      : Array.isArray(label) && label.length === 2 && label.every((part) => typeof part === "string")
        ? label
        : null;
    const kind = action?.type;
    const payload = action?.payload;
    const validLabel = labelParts !== null && labelParts.every((part) => (
      typeof part === "string"
      && part.length > 0
      && part.length <= 1_024
      && [...part].length <= 160
      && ![...part].some((character) => character < " " || character === "\u007f")
    ));
    const validPayload = kind === "text"
      ? typeof payload === "string"
        && new TextEncoder().encode(payload).byteLength <= 48 * 1_024
        && !payload.includes("\0")
      : kind === "img"
        ? typeof payload === "string"
          && payload.startsWith("data:image/png;base64,iVBORw0KGgo")
          && payload.length <= PLUGIN_BRIDGE_MAX_IMAGE_DATA_URL_CHARS
        : kind === "files"
          ? Array.isArray(payload)
            && payload.length >= 1
            && payload.length <= 16
            && payload.every((path) => typeof path === "string" && path.length > 0 && path.length <= 8_192)
          : false;
    if (
      !params
      || !hasOnlyKeys(params, new Set(["label", "action"]))
      || !action
      || !hasOnlyKeys(action, new Set(["type", "payload"]))
      || !validLabel
      || !validPayload
    ) {
      return {
        ok: false,
        error: "uTools redirect requires one bounded label and text, PNG, or file payload.",
        responseId,
      };
    }
  }
  if (isUtoolsSimulation) {
    const params = isPlainRecord(request.params) ? request.params : null;
    if (method === "compatibility.utools.simulate.keyboardTap") {
      const key = params?.key;
      const modifiers = params?.modifiers;
      if (
        !params
        || !hasOnlyKeys(params, new Set(["key", "modifiers"]))
        || typeof key !== "string"
        || key.length === 0
        || [...key].length > 32
        || [...key].some((character) => character < " " || character === "\u007f")
        || !Array.isArray(modifiers)
        || modifiers.length > 4
        || modifiers.some((modifier) => (
          typeof modifier !== "string"
          || !["control", "ctrl", "shift", "option", "alt", "command", "super", "meta"]
            .includes(modifier.trim().toLowerCase())
        ))
      ) {
        return {
          ok: false,
          error: "uTools keyboard simulation requires one bounded key and valid modifiers.",
          responseId,
        };
      }
    } else {
      const hasX = params ? Object.prototype.hasOwnProperty.call(params, "x") : false;
      const hasY = params ? Object.prototype.hasOwnProperty.call(params, "y") : false;
      const requiresPoint = method === "compatibility.utools.simulate.mouseMove";
      const validCoordinate = (value: unknown) => (
        typeof value === "number"
        && Number.isSafeInteger(value)
        && value >= -2_147_483_648
        && value <= 2_147_483_647
      );
      if (
        !params
        || !hasOnlyKeys(params, new Set(["x", "y"]))
        || hasX !== hasY
        || (requiresPoint && !hasX)
        || (hasX && (!validCoordinate(params.x) || !validCoordinate(params.y)))
      ) {
        return {
          ok: false,
          error: "uTools mouse simulation requires either no point or two 32-bit integer coordinates.",
          responseId,
        };
      }
    }
  }
  if (isUtoolsBrowser) {
    const params = isPlainRecord(request.params) ? request.params : null;
    const browserId = params?.browserId;
    const validBrowserId = (value: unknown) => typeof value === "string"
      && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value);
    const validChannel = (value: unknown) => typeof value === "string"
      && value.length > 0
      && [...value].length <= 128
      && ![...value].some((character) => character < " " || character === "\u007f");
    const validArgs = (value: unknown) => Array.isArray(value) && value.length <= 32;
    let valid = false;
    if (method === "compatibility.utools.browser.create") {
      valid = !!params
        && hasOnlyKeys(params, new Set(["url", "options"]))
        && typeof params.url === "string"
        && params.url.length > 0
        && [...params.url].length <= 2_048
        && ![...params.url].some((character) => character < " " || character === "\u007f")
        && isPlainRecord(params.options);
    } else if (method === "compatibility.utools.browser.control") {
      valid = !!params
        && hasOnlyKeys(params, new Set(["browserId", "action", "args"]))
        && validBrowserId(browserId)
        && typeof params.action === "string"
        && params.action.length > 0
        && params.action.length <= 40
        && Array.isArray(params.args)
        && params.args.length <= 4;
    } else if (method === "compatibility.utools.browser.send") {
      valid = !!params
        && hasOnlyKeys(params, new Set(["browserId", "channel", "args"]))
        && validBrowserId(browserId)
        && validChannel(params.channel)
        && validArgs(params.args);
    } else if (method === "compatibility.utools.browser.sendToParent") {
      valid = !!params
        && hasOnlyKeys(params, new Set(["channel", "args"]))
        && validChannel(params.channel)
        && validArgs(params.args);
    } else if (method === "compatibility.utools.browser.executeJavaScript") {
      valid = !!params
        && hasOnlyKeys(params, new Set(["browserId", "script"]))
        && validBrowserId(browserId)
        && typeof params.script === "string"
        && params.script.length > 0
        && [...params.script].length <= 65_536;
    } else if (method === "compatibility.utools.browser.executeResult") {
      valid = !!params
        && hasOnlyKeys(params, new Set(["requestId", "ok", "result", "error"]))
        && validBrowserId(params.requestId)
        && typeof params.ok === "boolean"
        && (params.error === null || (typeof params.error === "string" && [...params.error].length <= 2_000));
    } else if (method === "compatibility.utools.browser.closeSelf") {
      valid = !!params && hasOnlyKeys(params, new Set());
    }
    if (!valid) {
      return {
        ok: false,
        error: "uTools BrowserWindow Bridge parameters are invalid.",
        responseId,
      };
    }
  }
  if (isUtoolsUBrowser) {
    const params = isPlainRecord(request.params) ? request.params : null;
    const instanceId = params?.instanceId;
    const steps = params?.steps;
    const options = params?.options;
    const operations = new Set([
      "goto", "useragent", "viewport", "hide", "show", "css", "evaluate", "press",
      "click", "mousedown", "mouseup", "dblclick", "hover", "file", "drop", "input",
      "value", "check", "focus", "scroll", "download", "paste", "screenshot", "markdown",
      "pdf", "device", "wait", "when", "end", "devTools", "cookies", "setCookies",
      "removeCookies", "clearCookies",
    ]);
    const validInstanceId = instanceId === null || (
      typeof instanceId === "string"
      && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(instanceId)
    );
    const validSteps = Array.isArray(steps)
      && steps.length >= 1
      && steps.length <= 128
      && steps.every((step) => isPlainRecord(step)
        && hasOnlyKeys(step, new Set(["op", "args"]))
        && typeof step.op === "string"
        && operations.has(step.op)
        && Array.isArray(step.args)
        && step.args.length <= 8);
    if (
      !params
      || !hasOnlyKeys(params, new Set(["instanceId", "steps", "options"]))
      || !validInstanceId
      || !validSteps
      || !isPlainRecord(options)
    ) {
      return {
        ok: false,
        error: "uTools ubrowser accepts one bounded declarative chain and host-issued instance ID.",
        responseId,
      };
    }
  }
  if (method === "compatibility.utools.ubrowser.setProxy") {
    const params = isPlainRecord(request.params) ? request.params : null;
    const config = isPlainRecord(params?.config) ? params.config : null;
    if (
      !params
      || !hasOnlyKeys(params, new Set(["config"]))
      || !config
      || !hasOnlyKeys(config, new Set(["proxyRules", "proxyBypassRules"]))
      || typeof config.proxyRules !== "string"
      || config.proxyRules.length === 0
      || [...config.proxyRules].length > 2_048
      || (config.proxyBypassRules !== undefined && (
        typeof config.proxyBypassRules !== "string"
        || [...config.proxyBypassRules].length > 2_048
      ))
    ) {
      return {
        ok: false,
        error: "uTools ubrowser proxy config must contain bounded proxyRules strings.",
        responseId,
      };
    }
  }
  if (method === "compatibility.utools.ubrowser.clearCache") {
    const params = isPlainRecord(request.params) ? request.params : null;
    if (!params || !hasOnlyKeys(params, new Set())) {
      return {
        ok: false,
        error: "uTools clearUBrowserCache does not accept parameters.",
        responseId,
      };
    }
  }
  if (isUtoolsTool) {
    const params = isPlainRecord(request.params) ? request.params : null;
    const validName = (value: unknown) => typeof value === "string"
      && /^[a-z][a-z0-9]*(?:_[a-z0-9]+)*$/.test(value)
      && [...value].length <= 64;
    const validRequestId = (value: unknown) => typeof value === "string"
      && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value);
    let valid = false;
    if (method === "compatibility.utools.tools.register") {
      valid = !!params
        && hasOnlyKeys(params, new Set(["name"]))
        && validName(params.name);
    } else if (method === "compatibility.utools.tools.complete") {
      valid = !!params
        && hasOnlyKeys(params, new Set(["requestId", "name", "ok", "result", "error"]))
        && validRequestId(params.requestId)
        && validName(params.name)
        && typeof params.ok === "boolean"
        && Object.prototype.hasOwnProperty.call(params, "result")
        && (params.error === null || (
          typeof params.error === "string" && [...params.error].length <= 2_000
        ));
    } else if (method === "compatibility.utools.tools.progress") {
      const progress = params?.progress;
      const total = params?.total;
      const message = params?.message;
      valid = !!params
        && hasOnlyKeys(params, new Set(["requestId", "name", "progress", "total", "message"]))
        && validRequestId(params.requestId)
        && validName(params.name)
        && typeof progress === "number"
        && Number.isFinite(progress)
        && progress >= 0
        && (total === null || total === undefined || (
          typeof total === "number" && Number.isFinite(total) && total > 0 && total >= progress
        ))
        && (message === null || message === undefined || (
          typeof message === "string" && [...message].length <= 1_000 && !message.includes("\0")
        ));
    }
    if (!valid) {
      return {
        ok: false,
        error: "uTools MCP Bridge parameters are invalid.",
        responseId,
      };
    }
  }
  if (isUtoolsAi) {
    const params = isPlainRecord(request.params) ? request.params : null;
    const validRequestId = (value: unknown) => typeof value === "string"
      && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value);
    const validFunctionName = (value: unknown) => typeof value === "string"
      && /^[A-Za-z_][A-Za-z0-9_]{0,63}$/.test(value);
    const validMessage = (value: unknown) => {
      if (!isPlainRecord(value)
        || !hasOnlyKeys(value, new Set(["role", "content", "reasoning_content"]))
        || !["system", "user", "assistant"].includes(String(value.role))) {
        return false;
      }
      const content = value.content;
      const reasoning = value.reasoning_content;
      const validText = (text: unknown) => text === undefined || (
        typeof text === "string" && [...text].length <= 256 * 1024 && !text.includes("\0")
      );
      return validText(content)
        && validText(reasoning)
        && (typeof content === "string" || typeof reasoning === "string");
    };
    const validTool = (value: unknown) => {
      if (!isPlainRecord(value)
        || !hasOnlyKeys(value, new Set(["type", "function"]))
        || value.type !== "function"
        || !isPlainRecord(value.function)) {
        return false;
      }
      const fn = value.function;
      return hasOnlyKeys(fn, new Set(["name", "description", "parameters", "required"]))
        && validFunctionName(fn.name)
        && typeof fn.description === "string"
        && [...fn.description].length > 0
        && [...fn.description].length <= 1_000
        && isPlainRecord(fn.parameters)
        && (fn.required === undefined || (
          Array.isArray(fn.required)
          && fn.required.length <= 128
          && fn.required.every((field) => typeof field === "string" && field.length > 0 && [...field].length <= 160)
        ));
    };
    let valid = false;
    if (method === "compatibility.utools.ai.models") {
      valid = !!params && hasOnlyKeys(params, new Set());
    } else if (method === "compatibility.utools.ai.abort") {
      valid = !!params
        && hasOnlyKeys(params, new Set(["requestId"]))
        && validRequestId(params.requestId);
    } else if (method === "compatibility.utools.ai.start") {
      const option = params && isPlainRecord(params.option) ? params.option : null;
      valid = !!params
        && hasOnlyKeys(params, new Set(["requestId", "option", "stream"]))
        && validRequestId(params.requestId)
        && typeof params.stream === "boolean"
        && !!option
        && hasOnlyKeys(option, new Set(["model", "messages", "tools"]))
        && (option.model === undefined || (
          typeof option.model === "string" && option.model.length > 0 && [...option.model].length <= 320
        ))
        && Array.isArray(option.messages)
        && option.messages.length > 0
        && option.messages.length <= 128
        && option.messages.every(validMessage)
        && (option.tools === undefined || (
          Array.isArray(option.tools) && option.tools.length <= 64 && option.tools.every(validTool)
        ));
    } else if (method === "compatibility.utools.ai.toolComplete") {
      valid = !!params
        && hasOnlyKeys(params, new Set(["requestId", "invocationId", "name", "ok", "result", "error"]))
        && validRequestId(params.requestId)
        && validRequestId(params.invocationId)
        && validFunctionName(params.name)
        && typeof params.ok === "boolean"
        && Object.prototype.hasOwnProperty.call(params, "result")
        && (params.error === null || (
          typeof params.error === "string" && [...params.error].length <= 2_000
        ));
    }
    if (!valid) {
      return {
        ok: false,
        error: "uTools AI Bridge parameters are invalid.",
        responseId,
      };
    }
  }
  if (isUtoolsFfmpeg) {
    const params = isPlainRecord(request.params) ? request.params : null;
    const validRequestId = (value: unknown) => typeof value === "string"
      && /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(value);
    let valid = false;
    if (method === "compatibility.utools.ffmpeg.start") {
      const args = params?.args;
      let totalBytes = 0;
      valid = !!params
        && hasOnlyKeys(params, new Set(["requestId", "args"]))
        && validRequestId(params.requestId)
        && Array.isArray(args)
        && args.length > 0
        && args.length <= 256
        && args.every((arg) => {
          if (typeof arg !== "string" || arg.length === 0 || [...arg].some((character) => character < " " || character === "\u007f")) {
            return false;
          }
          const bytes = new TextEncoder().encode(arg).byteLength;
          totalBytes += bytes;
          return bytes <= 8 * 1024 && totalBytes <= 64 * 1024;
        });
    } else if (method === "compatibility.utools.ffmpeg.kill" || method === "compatibility.utools.ffmpeg.quit") {
      valid = !!params
        && hasOnlyKeys(params, new Set(["requestId"]))
        && validRequestId(params.requestId);
    }
    if (!valid) {
      return {
        ok: false,
        error: "uTools FFmpeg Bridge parameters are invalid.",
        responseId,
      };
    }
  }
  if (isUtoolsSharp) {
    const params = isPlainRecord(request.params) ? request.params : null;
    const input = params && isPlainRecord(params.input) ? params.input : null;
    const output = params && isPlainRecord(params.output) ? params.output : null;
    const operations = params?.operations;
    const validBase64 = (value: unknown) => typeof value === "string"
      && value.length > 0
      && value.length <= Math.ceil((16 * 1024 * 1024) / 3) * 4 + 8
      && /^[A-Za-z0-9+/]*={0,2}$/.test(value);
    const validDimension = (value: unknown) => typeof value === "number"
      && Number.isSafeInteger(value)
      && value >= 1
      && value <= 16_384;
    const validInput = !!input && (
      input.kind === "bytes"
        ? hasOnlyKeys(input, new Set(["kind", "dataBase64"])) && validBase64(input.dataBase64)
        : input.kind === "path"
          ? hasOnlyKeys(input, new Set(["kind", "path"]))
            && typeof input.path === "string"
            && input.path.length > 0
            && [...input.path].length <= 1_024
            && ![...input.path].some((character) => character < " " || character === "\u007f")
          : input.kind === "raw"
            ? hasOnlyKeys(input, new Set(["kind", "dataBase64", "width", "height", "channels"]))
              && validBase64(input.dataBase64)
              && validDimension(input.width)
              && validDimension(input.height)
              && [3, 4].includes(Number(input.channels))
            : input.kind === "create"
              && hasOnlyKeys(input, new Set(["kind", "width", "height", "channels", "background"]))
              && validDimension(input.width)
              && validDimension(input.height)
              && [3, 4].includes(Number(input.channels))
    );
    const validOutput = !!output && (
      ["buffer", "metadata"].includes(String(output.kind))
        ? hasOnlyKeys(output, new Set(["kind"]))
        : output.kind === "file"
          && hasOnlyKeys(output, new Set(["kind", "path"]))
          && typeof output.path === "string"
          && output.path.length > 0
          && [...output.path].length <= 1_024
    );
    const validOperations = Array.isArray(operations)
      && operations.length <= 48
      && operations.every((operation) => isPlainRecord(operation)
        && hasOnlyKeys(operation, new Set(["method", "args"]))
        && typeof operation.method === "string"
        && /^[A-Za-z0-9_]{1,48}$/.test(operation.method)
        && Array.isArray(operation.args)
        && operation.args.length <= 16);
    if (
      !params
      || !hasOnlyKeys(params, new Set(["input", "operations", "output"]))
      || !validInput
      || !validOutput
      || !validOperations
    ) {
      return {
        ok: false,
        error: "uTools Sharp requires one bounded declarative image pipeline.",
        responseId,
      };
    }
  }
  let maxJsonBytes = PLUGIN_BRIDGE_MAX_JSON_BYTES;
  if (isImageCopy || isUtoolsRedirect) {
    maxJsonBytes = PLUGIN_BRIDGE_MAX_IMAGE_JSON_BYTES;
  } else if (isDbWrite) {
    maxJsonBytes = PLUGIN_BRIDGE_MAX_DB_JSON_BYTES;
  } else if (isAttachmentWrite) {
    maxJsonBytes = PLUGIN_BRIDGE_MAX_ATTACHMENT_JSON_BYTES;
  } else if (isDbAllDocs) {
    maxJsonBytes = PLUGIN_BRIDGE_MAX_DB_QUERY_JSON_BYTES;
  } else if (isCryptoStorageWrite) {
    maxJsonBytes = PLUGIN_BRIDGE_MAX_CRYPTO_STORAGE_JSON_BYTES;
  } else if (isUtoolsBrowser) {
    maxJsonBytes = PLUGIN_BRIDGE_MAX_BROWSER_JSON_BYTES;
  } else if (isUtoolsUBrowser) {
    maxJsonBytes = PLUGIN_BRIDGE_MAX_UBROWSER_JSON_BYTES;
  } else if (isUtoolsTool) {
    maxJsonBytes = PLUGIN_BRIDGE_MAX_UTOOLS_TOOL_JSON_BYTES;
  } else if (isUtoolsAi) {
    maxJsonBytes = PLUGIN_BRIDGE_MAX_UTOOLS_AI_JSON_BYTES;
  } else if (isUtoolsSharp) {
    maxJsonBytes = PLUGIN_BRIDGE_MAX_UTOOLS_SHARP_JSON_BYTES;
  }
  const maxJsonNodes = isDbWrite
    ? PLUGIN_BRIDGE_MAX_DB_JSON_NODES
    : isUtoolsUBrowser
      ? PLUGIN_BRIDGE_MAX_UBROWSER_JSON_NODES
      : isUtoolsTool
        ? PLUGIN_BRIDGE_MAX_UTOOLS_TOOL_JSON_NODES
        : isUtoolsAi
          ? PLUGIN_BRIDGE_MAX_UTOOLS_AI_JSON_NODES
          : isUtoolsSharp
            ? PLUGIN_BRIDGE_MAX_UTOOLS_SHARP_JSON_NODES
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
    || method === "compatibility.utools.window.redirect"
    || method === "compatibility.utools.db.put"
    || method === "compatibility.utools.db.bulkDocs"
    || method === "compatibility.utools.db.postAttachment"
    || method === "compatibility.utools.dbCryptoStorage.set"
    || method === "compatibility.utools.browser.executeJavaScript"
    || method === "compatibility.utools.browser.executeResult"
    || method === "compatibility.utools.browser.send"
    || method === "compatibility.utools.browser.sendToParent"
    || method === "compatibility.utools.ubrowser.run"
    || method === "compatibility.utools.tools.complete"
    || method === "compatibility.utools.ai.start"
    || method === "compatibility.utools.ai.toolComplete"
    || method === "compatibility.utools.sharp.execute";
}
