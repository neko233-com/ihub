import type {
  BootstrapOptions,
  RuntimeCommandDefinition,
  CommandHandler,
  CommandInvocation,
  CommandResult,
  BatchRenamePreview,
  BatchRenameResult,
  ClipboardHistorySnapshot,
  CursorColorSample,
  DevelopmentBridge,
  Disposable,
  PluginProjectCreated,
  HostCallOptions,
  HostBridge,
  HostRequest,
  InjectedHostApi,
  Json,
  LauncherContextPayload,
  FilesystemDirectorySelection,
  FilesystemFileSelection,
  NotificationOptions,
  PluginNativeCommandResult,
  PluginContext,
  ScreenCaptureFocusLease,
  ScreenCaptureFocusLeaseRelease,
  PluginSettingWriteResult,
  PluginSubInputChange,
  PluginSubInputChangeHandler,
  WindowManagementAction,
  WindowManagementResult,
  SearchHandler,
  SearchProviderDefinition,
  SearchRequest,
  SearchResult,
  Unlisten,
} from "./types.js";
import { installUToolsCompatibility } from "./compatibility.js";

declare global {
  interface Window {
    __IHUB_PLUGIN_API__?: InjectedHostApi;
  }
}

const json = (value: unknown): Json => value as Json;

const FRAME_REQUEST_CHANNEL = "ihub-plugin-bridge/v1";
const FRAME_RESPONSE_CHANNEL = "ihub-host-bridge/v1";
const FRAME_CALL_TIMEOUT_MS = 30_000;
// The host asks for a visible confirmation and macOS may show its first
// Screen Recording permission panel. This changes only the iframe's wait; it
// does not make the host sample more than one user-approved pixel.
const CURSOR_COLOR_FRAME_CALL_TIMEOUT_MS = 2 * 60 * 1_000;
const MIN_NATIVE_COMMAND_FRAME_CALL_TIMEOUT_MS = 1_000;
// The host caps command policy at 30 minutes. The iframe grace only keeps the
// response channel alive long enough to receive the host's result; it never
// extends the host-owned deadline or any permission.
const NATIVE_COMMAND_FRAME_CALL_TIMEOUT_MS = 30 * 60 * 1_000 + 10_000;
const MAX_SUB_INPUT_PLACEHOLDER_LENGTH = 160;
const MAX_SUB_INPUT_VALUE_LENGTH = 4_096;

const eventName = (pluginId: string, kind: "command" | "search") => `ihub://plugin/${pluginId}/${kind}`;

const errorMessage = (error: unknown) => (error instanceof Error ? error.message : String(error));

interface PendingFrameCall {
  resolve: (value: unknown) => void;
  reject: (reason: Error) => void;
  timeout: number;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return Boolean(value) && typeof value === "object";
}

function frameCallTimeout(request: HostRequest, options?: HostCallOptions): number {
  if (request.method === "cursorColor.sampleOnce") {
    return CURSOR_COLOR_FRAME_CALL_TIMEOUT_MS;
  }

  // A plugin may only alter its own browser-side wait for a native command.
  // Every other bridge call remains at the normal 30-second timeout.
  if (request.method !== "native.runCommand") {
    return FRAME_CALL_TIMEOUT_MS;
  }

  const requested = options?.timeoutMs;
  if (typeof requested !== "number" || !Number.isFinite(requested)) {
    return NATIVE_COMMAND_FRAME_CALL_TIMEOUT_MS;
  }

  return Math.min(
    NATIVE_COMMAND_FRAME_CALL_TIMEOUT_MS,
    Math.max(MIN_NATIVE_COMMAND_FRAME_CALL_TIMEOUT_MS, Math.floor(requested)),
  );
}

/**
 * Creates the production bridge used by a plugin iframe. The parent window
 * fixes the plugin id and forwards only this small request envelope to Rust,
 * so normal plugin bundles do not need the Tauri API. iHub serves each iframe
 * from a separate loopback remote origin and verifies that origin in the
 * parent; native plugin workers remain outside that TypeScript boundary.
 */
function createFrameBridge(): HostBridge {
  const hostWindow = window.parent;
  const pending = new Map<string, PendingFrameCall>();
  const listeners = new Map<string, Set<(payload: unknown) => void | Promise<void>>>();
  let sequence = 0;

  const onMessage = (event: MessageEvent<unknown>) => {
    if (event.source !== hostWindow || !isRecord(event.data)) {
      return;
    }
    const message = event.data;
    if (message.channel !== FRAME_RESPONSE_CHANNEL) {
      return;
    }

    if (message.type === "event" && typeof message.name === "string") {
      const callbacks = [...(listeners.get(message.name) ?? [])];
      void Promise.all(callbacks.map((callback) => callback(message.payload)));
      return;
    }

    if (message.type !== "response" || typeof message.id !== "string") {
      return;
    }
    const call = pending.get(message.id);
    if (!call) {
      return;
    }
    pending.delete(message.id);
    window.clearTimeout(call.timeout);
    if (message.ok === true) {
      call.resolve(message.result);
    } else {
      call.reject(new Error(typeof message.error === "string" ? message.error : "iHub host call failed."));
    }
  };

  window.addEventListener("message", onMessage);

  return {
    call<T>(request: HostRequest, options?: HostCallOptions): Promise<T> {
      return new Promise<T>((resolve, reject) => {
        const id =
          "frame-" +
          Date.now().toString(36) +
          "-" +
          (sequence++).toString(36);
        const timeout = window.setTimeout(() => {
          pending.delete(id);
          reject(new Error("iHub host call timed out."));
        }, frameCallTimeout(request, options));
        pending.set(id, {
          resolve: (value) => resolve(value as T),
          reject,
          timeout,
        });
        hostWindow.postMessage(
          {
            channel: FRAME_REQUEST_CHANNEL,
            type: "call",
            id,
            request,
          },
          "*",
        );
      });
    },
    async listen<T>(name: string, listener: (payload: T) => void | Promise<void>): Promise<Unlisten> {
      const callbacks = listeners.get(name) ?? new Set();
      const wrapped = listener as (payload: unknown) => void | Promise<void>;
      callbacks.add(wrapped);
      listeners.set(name, callbacks);
      return () => {
        callbacks.delete(wrapped);
        if (callbacks.size === 0) {
          listeners.delete(name);
        }
      };
    },
  };
}

/** Returns true when the page is running inside an iHub plugin WebView. */
export function hasIHubHost(): boolean {
  if (typeof window === "undefined") {
    return false;
  }

  return Boolean(window.__IHUB_PLUGIN_API__ || window.parent !== window);
}

/**
 * Finds the production iHub bridge. iHub normally supplies a parent-frame
 * bridge; an injected bridge remains available to alternate host surfaces.
 */
export function getHostBridge(): HostBridge {
  if (typeof window === "undefined") {
    throw new Error("iHub plugins need a browser WebView or an explicit HostBridge.");
  }

  if (window.__IHUB_PLUGIN_API__) {
    return window.__IHUB_PLUGIN_API__;
  }

  if (window.parent !== window) {
    return createFrameBridge();
  }

  throw new Error(
    "The iHub host bridge was not found. Pass createDevelopmentBridge() to bootstrapPlugin() for a browser-only preview.",
  );
}

/**
 * A deliberately small in-memory bridge for unit tests and Vite previews.
 * It is not a security model and must never be shipped as a host replacement.
 */
export function createDevelopmentBridge(): DevelopmentBridge {
  const listeners = new Map<string, Set<(payload: unknown) => void | Promise<void>>>();
  const settings = new Map<string, Json>();
  let subInputActive = false;

  return {
    async call<T>(request: HostRequest): Promise<T> {
      const params = (request.params ?? {}) as Record<string, Json>;
      switch (request.method) {
        case "settings.get":
          return (settings.get(String(params.key)) ?? params.fallback) as T;
        case "settings.set":
          settings.set(String(params.key), params.value);
          return { saved: true, persistent: false } as T;
        case "ui.subInput.set":
          subInputActive = true;
          return true as T;
        case "ui.subInput.remove":
          subInputActive = false;
          return true as T;
        case "ui.subInput.setValue": {
          if (!subInputActive) {
            return false as T;
          }
          const handlers = [
            ...(listeners.get(`ihub://plugin/${request.pluginId}/event/subInput.change`) ?? []),
          ];
          await Promise.all(handlers.map((handler) => handler({ text: String(params.value ?? "") })));
          return true as T;
        }
        case "clipboard.readText":
          return "" as T;
        case "clipboard.history.snapshot":
          return { enabled: false, items: [] } as T;
        case "filesystem.selectDirectory":
        case "filesystem.selectFiles":
        case "filesystem.batchRename.preview":
        case "filesystem.batchRename.apply":
        case "developer.createProject":
          throw new Error("Filesystem bridge calls require the iHub desktop host.");
        case "screenCapture.acquireFocusLease":
        case "screenCapture.releaseFocusLease":
          throw new Error("Screen-capture focus leases require the iHub desktop host.");
        case "cursorColor.sampleOnce":
          throw new Error("Native cursor color sampling requires the iHub desktop host.");
        case "native.runCommand":
          throw new Error("Native plugin commands require the iHub desktop host.");
        case "window.manageLauncher":
          throw new Error("Launcher layout actions require the iHub desktop host.");
        case "launcherContext.consume":
          throw new Error("Launcher context transfers require an explicit iHub desktop-host action.");
        default:
          return undefined as T;
      }
    },
    async listen<T>(name: string, listener: (payload: T) => void | Promise<void>): Promise<Unlisten> {
      const handlers = listeners.get(name) ?? new Set();
      const wrapped = listener as (payload: unknown) => void | Promise<void>;
      handlers.add(wrapped);
      listeners.set(name, handlers);
      return () => {
        handlers.delete(wrapped);
        if (handlers.size === 0) {
          listeners.delete(name);
        }
      };
    },
    async emit<T>(name: string, payload: T): Promise<void> {
      const handlers = [...(listeners.get(name) ?? [])];
      await Promise.all(handlers.map((handler) => handler(payload)));
    },
  };
}

class Runtime implements Disposable {
  readonly context: PluginContext;
  private readonly commandHandlers = new Map<string, CommandHandler>();
  private readonly searchHandlers = new Map<string, SearchHandler>();
  private readonly unlisten: Unlisten[] = [];
  private compatibility: Disposable | null = null;
  private subInputOperation: Promise<void> = Promise.resolve();
  private commandListenerReady = false;
  private searchListenerReady = false;
  private subInputListenerReady = false;
  private subInputHandler: PluginSubInputChangeHandler | null = null;
  private disposed = false;

  constructor(
    private readonly pluginId: string,
    private readonly bridge: HostBridge,
    private readonly onError: (error: unknown) => void,
  ) {
    this.context = {
      pluginId,
      commands: {
        register: (definition, handler) => this.registerCommand(definition, handler),
        execute: (commandId, input) => this.call("commands.execute", { commandId, input }),
      },
      search: {
        register: (definition, handler) => this.registerSearchProvider(definition, handler),
      },
      subInput: {
        set: (onChange, placeholder, focus) =>
          this.enqueueSubInput(() => this.setSubInput(onChange, placeholder, focus)),
        remove: () => this.enqueueSubInput(() => this.removeSubInput()),
        setValue: (value) => this.enqueueSubInput(() => this.setSubInputValue(value)),
      },
      settings: {
        get: <T extends Json>(key: string, fallback?: T) => this.getSetting(key, fallback),
        set: (key, value) => this.call<PluginSettingWriteResult>("settings.set", { key, value }),
      },
      clipboard: {
        readText: () => this.call<string>("clipboard.readText"),
        writeText: (value) => this.call("clipboard.writeText", { value }),
        history: {
          snapshot: () => this.call<ClipboardHistorySnapshot>("clipboard.history.snapshot"),
        },
      },
      shell: {
        openExternal: (url) => this.call("shell.openExternal", { url }),
        openPath: (grantId) => this.call("shell.openPath", { grantId }),
      },
      screenCapture: {
        acquireFocusLease: () => this.call<ScreenCaptureFocusLease>("screenCapture.acquireFocusLease"),
        releaseFocusLease: (leaseId) =>
          this.call<ScreenCaptureFocusLeaseRelease>("screenCapture.releaseFocusLease", { leaseId }),
      },
      cursorColor: {
        sampleOnce: () => this.call<CursorColorSample>("cursorColor.sampleOnce"),
      },
      windowManagement: {
        manageLauncher: (action: WindowManagementAction) =>
          this.call<WindowManagementResult>("window.manageLauncher", { action }),
      },
      launcherContext: {
        consume: (contextId) =>
          this.call<LauncherContextPayload>("launcherContext.consume", { contextId }),
      },
      filesystem: {
        selectDirectory: () => this.call<FilesystemDirectorySelection>("filesystem.selectDirectory"),
        selectFiles: () => this.call<FilesystemFileSelection>("filesystem.selectFiles"),
        previewBatchRename: (options) => this.call<BatchRenamePreview>("filesystem.batchRename.preview", options),
        applyBatchRename: (options) => this.call<BatchRenameResult>("filesystem.batchRename.apply", options),
      },
      native: {
        runCommand: (options) =>
          this.call<PluginNativeCommandResult>("native.runCommand", options, {
            timeoutMs: NATIVE_COMMAND_FRAME_CALL_TIMEOUT_MS,
          }),
      },
      developer: {
        createProject: (options) => this.call<PluginProjectCreated>("developer.createProject", options),
      },
      events: {
        on: (name, listener) => this.on(name, listener),
      },
      notifications: {
        show: (options) => this.showNotification(options),
      },
      logger: {
        debug: (message, details) => this.log("debug", message, details),
        info: (message, details) => this.log("info", message, details),
        warn: (message, details) => this.log("warn", message, details),
        error: (message, details) => this.log("error", message, details),
      },
    };
  }

  async activate(activate: (context: PluginContext) => void | Promise<void>): Promise<void> {
    this.compatibility = installUToolsCompatibility(this.context, this.onError);
    await activate(this.context);
    await this.call("lifecycle.ready");
  }

  async dispose(): Promise<void> {
    if (this.disposed) {
      return;
    }
    this.disposed = true;
    this.compatibility?.dispose();
    this.compatibility = null;
    const removeSubInput = this.subInputHandler
      ? this.bridge.call({
          pluginId: this.pluginId,
          method: "ui.subInput.remove",
        }).catch(this.onError)
      : Promise.resolve();
    this.subInputHandler = null;
    this.commandHandlers.clear();
    this.searchHandlers.clear();
    await Promise.all([
      removeSubInput,
      ...this.unlisten.splice(0).map((dispose) => Promise.resolve(dispose())),
    ]);
    await this.bridge.call({ pluginId: this.pluginId, method: "lifecycle.dispose" }).catch(this.onError);
  }

  private async setSubInput(
    onChange: PluginSubInputChangeHandler,
    placeholder = "",
    focus = true,
  ): Promise<boolean> {
    this.assertActive();
    if (typeof onChange !== "function") {
      throw new Error("Sub-input change handler must be a function.");
    }
    if (typeof placeholder !== "string" || placeholder.length > MAX_SUB_INPUT_PLACEHOLDER_LENGTH) {
      throw new Error(`Sub-input placeholder must be at most ${MAX_SUB_INPUT_PLACEHOLDER_LENGTH} characters.`);
    }
    if (typeof focus !== "boolean") {
      throw new Error("Sub-input focus must be a boolean.");
    }

    await this.ensureSubInputListener();
    const previousHandler = this.subInputHandler;
    this.subInputHandler = onChange;
    try {
      const accepted = await this.call<boolean>("ui.subInput.set", {
        placeholder,
        focus,
      });
      if (accepted !== true) {
        this.subInputHandler = previousHandler;
        return false;
      }
      return true;
    } catch (error) {
      this.subInputHandler = previousHandler;
      throw error;
    }
  }

  private async removeSubInput(): Promise<boolean> {
    this.assertActive();
    const removed = await this.call<boolean>("ui.subInput.remove");
    if (removed === true) {
      this.subInputHandler = null;
      return true;
    }
    return false;
  }

  private async setSubInputValue(value: string): Promise<boolean> {
    this.assertActive();
    if (typeof value !== "string" || value.length > MAX_SUB_INPUT_VALUE_LENGTH) {
      throw new Error(`Sub-input value must be at most ${MAX_SUB_INPUT_VALUE_LENGTH} characters.`);
    }
    return (await this.call<boolean>("ui.subInput.setValue", { value })) === true;
  }

  private enqueueSubInput<T>(operation: () => Promise<T>): Promise<T> {
    const result = this.subInputOperation.then(operation);
    this.subInputOperation = result.then(
      () => undefined,
      () => undefined,
    );
    return result;
  }

  private async registerCommand(definition: RuntimeCommandDefinition, handler: CommandHandler): Promise<Disposable> {
    this.assertActive();
    if (this.commandHandlers.has(definition.id)) {
      throw new Error(`Command \"${definition.id}\" is already registered by ${this.pluginId}.`);
    }

    await this.ensureCommandListener();
    this.commandHandlers.set(definition.id, handler);
    // Runtime registrations must never turn iframe-provided strings into a
    // package artwork path or OS-level shortcut. Keep this defensive strip
    // even though the public RuntimeCommandDefinition type rejects both at
    // compile time.
    const { icon: _ignoredIcon, shortcut: _ignoredShortcut, ...hostDefinition } = definition as RuntimeCommandDefinition & {
      icon?: unknown;
      shortcut?: unknown;
    };
    await this.call("commands.register", { definition: json(hostDefinition) });
    return this.registrationDisposable(
      "commands.unregister",
      definition.id,
      this.commandHandlers,
      "commandId",
    );
  }

  private async registerSearchProvider(
    definition: SearchProviderDefinition,
    handler: SearchHandler,
  ): Promise<Disposable> {
    this.assertActive();
    if (this.searchHandlers.has(definition.id)) {
      throw new Error(`Search provider \"${definition.id}\" is already registered by ${this.pluginId}.`);
    }

    await this.ensureSearchListener();
    this.searchHandlers.set(definition.id, handler);
    await this.call("search.register", { definition: json(definition) });
    return this.registrationDisposable(
      "search.unregister",
      definition.id,
      this.searchHandlers,
      "providerId",
    );
  }

  private registrationDisposable(
    method: string,
    id: string,
    handlers: Map<string, unknown>,
    parameterName: string,
  ): Disposable {
    let disposed = false;
    return {
      dispose: async () => {
        if (disposed) {
          return;
        }
        disposed = true;
        handlers.delete(id);
        await this.call(method, { [parameterName]: id });
      },
    };
  }

  private async ensureCommandListener(): Promise<void> {
    if (this.commandListenerReady) {
      return;
    }
    const dispose = await this.bridge.listen<CommandInvocation>(eventName(this.pluginId, "command"), (request) =>
      this.handleCommand(request),
    );
    this.unlisten.push(dispose);
    this.commandListenerReady = true;
  }

  private async ensureSearchListener(): Promise<void> {
    if (this.searchListenerReady) {
      return;
    }
    const dispose = await this.bridge.listen<SearchRequest>(eventName(this.pluginId, "search"), (request) =>
      this.handleSearch(request),
    );
    this.unlisten.push(dispose);
    this.searchListenerReady = true;
  }

  private async ensureSubInputListener(): Promise<void> {
    if (this.subInputListenerReady) {
      return;
    }
    const dispose = await this.bridge.listen<PluginSubInputChange>(
      `ihub://plugin/${this.pluginId}/event/subInput.change`,
      async (change) => {
        if (
          !change
          || typeof change !== "object"
          || typeof change.text !== "string"
          || change.text.length > MAX_SUB_INPUT_VALUE_LENGTH
        ) {
          return;
        }
        const handler = this.subInputHandler;
        if (!handler) {
          return;
        }
        try {
          await handler({ text: change.text });
        } catch (error) {
          this.onError(error);
        }
      },
    );
    this.unlisten.push(dispose);
    this.subInputListenerReady = true;
  }

  private async handleCommand(request: CommandInvocation): Promise<void> {
    const handler = this.commandHandlers.get(request.commandId);
    if (!handler) {
      await this.respond("commands.complete", request.requestId, false, undefined, `Unknown command: ${request.commandId}`);
      return;
    }

    try {
      const result = await handler(request);
      await this.respond("commands.complete", request.requestId, true, result ?? {});
    } catch (error) {
      this.onError(error);
      await this.respond("commands.complete", request.requestId, false, undefined, errorMessage(error));
    }
  }

  private async handleSearch(request: SearchRequest): Promise<void> {
    const handler = this.searchHandlers.get(request.providerId);
    if (!handler) {
      await this.respond("search.complete", request.requestId, false, [], `Unknown search provider: ${request.providerId}`);
      return;
    }

    try {
      const results = await handler(request);
      await this.respond("search.complete", request.requestId, true, results);
    } catch (error) {
      this.onError(error);
      await this.respond("search.complete", request.requestId, false, [], errorMessage(error));
    }
  }

  private async respond(
    method: string,
    requestId: string,
    ok: boolean,
    result?: CommandResult | SearchResult[],
    error?: string,
  ): Promise<void> {
    await this.call(method, { requestId, ok, result: result === undefined ? null : json(result), error: error ?? null });
  }

  private async getSetting<T extends Json>(key: string, fallback?: T): Promise<T> {
    const value = await this.call<T | undefined>("settings.get", { key, fallback: fallback ?? null });
    return (value === undefined ? fallback : value) as T;
  }

  private async showNotification(options: NotificationOptions): Promise<void> {
    await this.call("notifications.show", options);
  }

  private async on<T>(name: string, listener: (payload: T) => void | Promise<void>): Promise<Disposable> {
    this.assertActive();
    const dispose = await this.bridge.listen<T>(`ihub://plugin/${this.pluginId}/event/${name}`, listener);
    this.unlisten.push(dispose);
    return { dispose };
  }

  private log(level: "debug" | "info" | "warn" | "error", message: string, details?: Json): void {
    void this.call("log", { level, message, details: details ?? null }).catch(this.onError);
  }

  private async call<T = void>(method: string, params?: unknown, options?: HostCallOptions): Promise<T> {
    this.assertActive();
    return this.bridge.call<T>(
      { pluginId: this.pluginId, method, params: params ? json(params) : undefined },
      options,
    );
  }

  private assertActive(): void {
    if (this.disposed) {
      throw new Error(`Plugin runtime for ${this.pluginId} has already been disposed.`);
    }
  }
}

/**
 * Boots a plugin frontend and returns a disposable runtime. The host calls the
 * registered command/search callbacks through the host bridge, never by
 * serializing JavaScript functions across IPC.
 */
export async function bootstrapPlugin(
  pluginId: string,
  activate: (context: PluginContext) => void | Promise<void>,
  options: BootstrapOptions = {},
): Promise<Disposable> {
  const onError = options.onError ?? ((error: unknown) => console.error(`[${pluginId}]`, error));
  const runtime = new Runtime(pluginId, options.bridge ?? getHostBridge(), onError);
  try {
    await runtime.activate(activate);
    return runtime;
  } catch (error) {
    await runtime.dispose();
    throw error;
  }
}
