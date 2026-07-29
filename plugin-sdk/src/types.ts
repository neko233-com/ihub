/** JSON values are the only values that cross the plugin-host IPC boundary. */
export type JsonPrimitive = string | number | boolean | null;
export type Json = JsonPrimitive | Json[] | { [key: string]: Json };

export type PluginTarget =
  | "windows-x86_64"
  | "windows-aarch64"
  | "darwin-x86_64"
  | "darwin-aarch64";

export interface PluginManifest {
  /** Optional editor-only path or URL for manifest JSON Schema validation. */
  $schema?: string;
  schemaVersion: 1;
  id: string;
  name: string;
  version: string;
  description?: string;
  /**
   * Preferred package-relative PNG, JPEG, or WebP identity artwork.
   * Invalid top-level artwork prevents the package from loading.
   */
  icon?: string;
  /** Compatibility alias for `icon`; manifests must not declare both. */
  logo?: string;
  author?: string;
  license?: string;
  homepage?: string;
  repository?: string;
  engines: {
    ihub: string;
    api: string;
  };
  entry: {
    frontend: string;
  };
  backend?: {
    protocol: "jsonl-rpc-v1";
    binaries: PluginBinary[];
    restart?: "never" | "on-failure" | "always";
  };
  activationEvents?: string[];
  contributes?: PluginContributions;
  permissions: PluginPermissions;
  update?: {
    channel?: "stable" | "beta";
    autoUpdate?: boolean;
  };
}

export interface PluginBinary {
  target: PluginTarget;
  path: string;
  args?: string[];
}

export interface PluginPermissions {
  filesystem?: {
    read?: string[];
    write?: string[];
  };
  network?: {
    allow?: string[];
  };
  clipboard?: {
    read?: boolean;
    write?: boolean;
    /** Read-only access to iHub's existing opt-in clipboard history. */
    history?: boolean;
  };
  process?: {
    spawn?: boolean;
    allow?: string[];
  };
  shell?: {
    openExternal?: boolean;
    openPath?: boolean;
  };
  /**
   * Lets the trusted host delegate display capture only to a native-validated
   * visible Surface lease, and allows a bounded focus-protection lease around
   * the browser's `getDisplayMedia` picker. Hidden runtimes receive no
   * delegation. It never bypasses the browser/OS picker, grants screen pixels,
   * or exposes a native capture API.
   */
  screenCapture?: boolean;
  /**
   * Lets the trusted host delegate microphone capture only to a
   * native-validated visible Surface lease. Hidden runtimes receive no
   * delegation. It never bypasses the browser/OS permission prompt, grants
   * audio samples to the host, or exposes a native recording API.
   */
  microphone?: boolean;
  /**
   * Allows one host-confirmed native sample of the pixel beneath the cursor.
   * It is not a screenshot, recording, coordinate, or background-polling
   * capability.
   */
  cursorColor?: boolean;
  globalShortcut?: boolean;
  notifications?: boolean;
  nativeApi?: boolean;
  /** Allows only fixed layout actions for iHub's own launcher window. */
  windowManagement?: boolean;
  /**
   * Allows the host to attach a bounded, one-shot launcher selection to an
   * explicitly chosen frontend command. This is not clipboard, filesystem,
   * or image-read access: the plugin receives text only when declared, file
   * metadata without paths, and opaque image handles without pixels.
   */
  launcherContext?: {
    /** Receive text that the person explicitly chose to send to this command. */
    text?: boolean;
    /** Receive canonical metadata and opaque handles for explicit file/folder selections. */
    files?: boolean;
    /** Receive an opaque handle plus metadata for an explicit pasted image. */
    image?: boolean;
  };
}

export interface PluginContributions {
  commands?: CommandDefinition[];
  searchProviders?: SearchProviderDefinition[];
  settings?: PluginSettingDefinition[];
  /**
   * Plugin-level, manifest-only accelerator mappings. Each binding opens one
   * declared command or pre-fills one bounded launcher keyword.
   */
  globalShortcuts?: GlobalShortcutDefinition[];
  quickActions?: QuickActionDefinition[];
}

export interface GlobalShortcutDefinition {
  id: string;
  shortcut: string;
  commandId?: string;
  keyword?: string;
}

export interface CommandDefinition {
  id: string;
  title: string;
  subtitle?: string;
  keywords?: string[];
  /**
   * Static manifest artwork only. The host accepts a safe package-relative
   * PNG, JPEG, or WebP path and normalizes usable content. An unavailable
   * legacy command icon falls back safely instead of invalidating the plugin.
   * Runtime command registration cannot introduce or replace artwork.
   */
  icon?: string;
  /**
   * Manifest-only shorthand for invoking this command from a host-owned
   * global accelerator. Requires permissions.globalShortcut.
   */
  shortcut?: string;
  /**
   * Opens the plugin iframe or starts its manifest-locked native worker.
   * Omit it for the compatible default: native when the plugin has a worker,
   * frontend otherwise.
   */
  execution?: "frontend" | "native";
  /**
   * Bounded execution policy for an explicitly native command. The desktop
   * host remains authoritative: this only declares the command's requested
   * deadline and cannot grant extra process or filesystem permissions.
   */
  run?: NativeCommandRunPolicy;
}

export interface NativeCommandRunPolicy {
  /** Host-enforced native-worker deadline in milliseconds (1,000–1,800,000). */
  timeoutMs: number;
}

/**
 * A command registered by a running frontend. Artwork is intentionally absent:
 * dynamic registration cannot send a package path or image payload to the host.
 * Declare command artwork statically in plugin.json instead.
 */
export type RuntimeCommandDefinition = Omit<CommandDefinition, "icon" | "shortcut"> & {
  readonly icon?: never;
  readonly shortcut?: never;
};

export interface SearchProviderDefinition {
  id: string;
  title: string;
  trigger?: string;
  priority?: number;
}

export interface PluginSettingDefinition {
  key: string;
  title: string;
  description?: string;
  type: "string" | "number" | "boolean" | "select" | "textarea";
  default?: Json;
  options?: Array<{
    label: string;
    value: string | number | boolean;
  }>;
  secret?: boolean;
}

export interface QuickActionDefinition {
  id: string;
  title: string;
  when?: string;
}

export interface HostRequest {
  pluginId: string;
  method: string;
  params?: Json;
}

/**
 * Browser-side wait options for one host call. They never change the host's
 * permission checks, command deadline, or any other authority.
 */
export interface HostCallOptions {
  timeoutMs?: number;
}

export type Unlisten = () => void | Promise<void>;

/**
 * A bridge is injected by iHub in production. The SDK also accepts a bridge
 * explicitly, which keeps plugins testable and makes browser-only previews
 * possible without a desktop host.
 */
export interface HostBridge {
  call<T = unknown>(request: HostRequest, options?: HostCallOptions): Promise<T>;
  listen<T = unknown>(event: string, listener: (payload: T) => void | Promise<void>): Promise<Unlisten>;
}

export interface InjectedHostApi {
  call<T = unknown>(request: HostRequest, options?: HostCallOptions): Promise<T>;
  listen<T = unknown>(event: string, listener: (payload: T) => void | Promise<void>): Promise<Unlisten>;
}

export interface Disposable {
  dispose(): void | Promise<void>;
}

export interface CommandInvocation {
  requestId: string;
  commandId: string;
  input?: Json;
  context?: Record<string, Json>;
  /**
   * Present only when the trusted iHub parent deliberately attached a
   * short-lived launcher-context transfer to this exact command invocation.
   * It is an opaque ID, not user content. Call `ihub.launcherContext.consume`
   * from the handler to take the payload exactly once.
   */
  launcherContext?: LauncherContextInvocation;
}

/** A host-issued, opaque reference to one deliberately attached launch context. */
export interface LauncherContextInvocation {
  contextId: string;
  /** Fixed host deadline; an expired ID cannot be refreshed or replayed. */
  expiresInMs: number;
}

/** Metadata for one canonical file or folder selected by the person. */
export interface LauncherContextFileMetadata {
  /** Opaque metadata identity. It is not a filesystem grant or path handle. */
  handleId: string;
  name: string;
  kind: "file" | "folder";
  /** Present for regular files only. */
  size?: number;
}

/** Metadata for a bounded image paste. No image bytes are exposed by this API. */
export interface LauncherContextImageHandle {
  /** Opaque identity only; it cannot be resolved into a path or pixel stream. */
  handleId: string;
  name: string;
  mimeType: "image/png";
  width: number;
  height: number;
}

/** The bounded payload returned only once from a valid context ID. */
export interface LauncherContextPayload {
  text?: string;
  files: LauncherContextFileMetadata[];
  image?: LauncherContextImageHandle;
}

export interface PluginLauncherContext {
  /**
   * Takes a host-issued launcher payload exactly once. The ID must come from
   * `CommandInvocation.launcherContext` for the command the person selected.
   * This never reads the live clipboard, resolves a local path, or returns
   * image bytes.
   */
  consume(contextId: string): Promise<LauncherContextPayload>;
}

export interface CommandResult {
  message?: string;
  data?: Json;
  close?: boolean;
}

export type CommandHandler = (invocation: CommandInvocation) => CommandResult | void | Promise<CommandResult | void>;

export interface SearchRequest {
  requestId: string;
  providerId: string;
  query: string;
  limit?: number;
  context?: Record<string, Json>;
}

export interface SearchResult {
  id: string;
  title: string;
  subtitle?: string;
  icon?: string;
  score?: number;
  payload?: Json;
  actions?: Array<{
    id: string;
    title: string;
  }>;
}

export type SearchHandler = (request: SearchRequest) => SearchResult[] | Promise<SearchResult[]>;

export interface NotificationOptions {
  title: string;
  body?: string;
  level?: "info" | "success" | "warning" | "error";
}

export interface PluginCommands {
  register(definition: RuntimeCommandDefinition, handler: CommandHandler): Promise<Disposable>;
  execute(commandId: string, input?: Json): Promise<void>;
}

export interface PluginSearch {
  register(definition: SearchProviderDefinition, handler: SearchHandler): Promise<Disposable>;
}

export interface PluginSettings {
  get<T extends Json = Json>(key: string, fallback?: T): Promise<T>;
  /** `persistent` is false for manifest-declared `secret` keys. */
  set(key: string, value: Json): Promise<PluginSettingWriteResult>;
}

export interface PluginSettingWriteResult {
    saved: true;
    persistent: boolean;
}

/** One text-only item from iHub's opt-in, host-owned clipboard history. */
export interface ClipboardHistoryItem {
  id: string;
  text: string;
  capturedAt: string;
  pinned: boolean;
}

/** A bounded read-only snapshot; requesting it never enables capture. */
export interface ClipboardHistorySnapshot {
  enabled: boolean;
  items: ClipboardHistoryItem[];
}

export interface PluginClipboardHistory {
  /** Requires the explicit `clipboard.history` manifest permission. */
  snapshot(): Promise<ClipboardHistorySnapshot>;
}

export interface PluginClipboard {
  readText(): Promise<string>;
  writeText(value: string): Promise<void>;
  readonly history: PluginClipboardHistory;
}

export interface PluginShell {
  openExternal(url: string): Promise<void>;
  openPath(path: string): Promise<void>;
}

/** An opaque host-owned lease that temporarily suspends launcher auto-hide. */
export interface ScreenCaptureFocusLease {
  leaseId: string;
  /** Fixed host deadline; release in `finally` rather than waiting for it. */
  expiresInMs: number;
}

export interface ScreenCaptureFocusLeaseRelease {
  /** False means the lease was already expired or released. */
  released: boolean;
}

export interface PluginScreenCapture {
  /**
   * Requires the explicit `screenCapture: true` manifest permission. Start
   * this request, then call browser `getDisplayMedia()` synchronously without
   * awaiting it so the user's transient activation is preserved.
   */
  acquireFocusLease(): Promise<ScreenCaptureFocusLease>;
  /**
   * Releases only a lease issued to this plugin. A lease from another plugin
   * is rejected by the host and remains active for its owner.
   */
  releaseFocusLease(leaseId: string): Promise<ScreenCaptureFocusLeaseRelease>;
}

/** A deliberately narrow projection of one native cursor-pixel sample. */
export interface CursorColorSample {
  hex: string;
  rgb: string;
}

export interface PluginCursorColor {
  /**
   * Requires `cursorColor: true`. The iHub host intercepts the call and asks
   * the user to confirm before a fixed two-second, one-pixel native sample.
   * It works only from the visible plugin surface, never a hidden runtime.
   */
  sampleOnce(): Promise<CursorColorSample>;
}

export type WindowManagementAction =
  | "center"
  | "snap-left"
  | "snap-right"
  | "toggle-always-on-top";

/** The result of a fixed layout action on iHub's own launcher window. */
export interface WindowManagementResult {
  action: WindowManagementAction;
  alwaysOnTop: boolean;
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface PluginWindowManagement {
  /** Requires `windowManagement: true`; never controls other applications. */
  manageLauncher(action: WindowManagementAction): Promise<WindowManagementResult>;
}

export interface PluginSubInputChange {
  text: string;
}

export type PluginSubInputChangeHandler = (
  change: PluginSubInputChange,
) => void | Promise<void>;

/**
 * Controls the text input rendered by the trusted visible iHub plugin host.
 * The input is tied to the current surface lease and is never available to a
 * hidden search runtime or a native worker.
 */
export interface PluginSubInput {
  /**
   * Creates or updates the host input and replaces the current change handler.
   * Focus defaults to true, matching the familiar uTools sub-input behavior.
   */
  set(
    onChange: PluginSubInputChangeHandler,
    placeholder?: string,
    focus?: boolean,
  ): Promise<boolean>;
  /** Removes the host input and its change handler. */
  remove(): Promise<boolean>;
  /**
   * Updates the host input. A successful update also invokes the registered
   * change handler with the resulting text.
   */
  setValue(value: string): Promise<boolean>;
}

/** A short-lived directory capability issued only after the native picker. */
export type FilesystemDirectorySelection =
  | { cancelled: true }
  | { cancelled: false; grantId: string; directory: string };

/** Metadata for a user-selected file. The host keeps the canonical path private. */
export interface FilesystemSelectedFile {
  name: string;
  size: number;
}

/** A short-lived file capability that can be passed only to `native.runCommand`. */
export type FilesystemFileSelection =
  | { cancelled: true }
  | { cancelled: false; grantId: string; files: FilesystemSelectedFile[] };

export interface BatchRenameItem {
  from: string;
  to: string;
}

/**
 * This preview is generated by the native host. `previewId` is absent when
 * validation found no safe batch to apply, and cannot be forged from items.
 */
export interface BatchRenamePreview {
  previewId: string | null;
  directory: string;
  items: BatchRenameItem[];
  canApply: boolean;
  errors: string[];
}

export interface BatchRenameResult {
  renamed: number;
  items: BatchRenameItem[];
}

/**
 * The host-created project path and the starter's local next steps. Creating
 * a project only writes a new directory; it never installs dependencies,
 * starts a dev server, or runs a generated/native script. The generated
 * `pnpm build` command includes a read-only pre-link check before a user
 * explicitly links the project in iHub.
 */
export interface PluginProjectCreated {
  projectPath: string;
  pluginId: string;
  nextSteps: string[];
}

export interface PluginFilesystem {
  /** Requires `filesystem.read: ["user-selected"]`. */
  selectDirectory(): Promise<FilesystemDirectorySelection>;
  /**
   * Requires `filesystem.read: ["user-selected"]`. The opaque grant can be
   * consumed only once by this plugin's `native.runCommand`; browser code
   * receives names and sizes, never local file paths.
   */
  selectFiles(): Promise<FilesystemFileSelection>;
  /** Requires a live directory grant and `filesystem.read: ["user-selected"]`. */
  previewBatchRename(options: {
    grantId: string;
    find: string;
    replace: string;
    useRegex?: boolean;
    /**
     * When `replace` contains `{n}`, the host substitutes a deterministic
     * sequence before applying literal or regex replacement. Defaults to 1.
     */
    sequenceStart?: number;
    /**
     * Minimum digit width for `{n}` (0–12). Defaults to 3, so `{n}` becomes
     * `001`, `002`, … and keeps lexical file order useful.
     */
    sequencePadding?: number;
  }): Promise<BatchRenamePreview>;
  /** Requires the same live grant, preview token, and write permission. */
  applyBatchRename(options: { grantId: string; previewId: string }): Promise<BatchRenameResult>;
}

/** The host waits for the plugin's one-shot declared worker and returns its bounded output. */
export interface PluginNativeCommandResult {
  pluginId: string;
  commandId: string;
  success: boolean;
  exitCode: number | null;
  stdout: string;
  stderr: string;
  output?: Json;
}

export interface PluginNative {
  /**
   * Requires `nativeApi: true`. With a file grant, the worker receives an
   * envelope `{ input, files }` containing canonical paths; the iframe never
   * sees those paths and the grant is consumed whether the worker succeeds or
   * fails.
   */
  runCommand(options: {
    commandId: string;
    input?: Json;
    fileGrantId?: string;
  }): Promise<PluginNativeCommandResult>;
}

export interface PluginDeveloper {
  /**
   * Requires both `filesystem.read` and `filesystem.write` for the exact
   * `user-selected` scope. The host resolves the opaque grant; it never
   * accepts a parent path from the plugin. The caller must present the
   * returned next steps and let the developer choose whether to run them.
   */
  createProject(options: { grantId: string; pluginId: string }): Promise<PluginProjectCreated>;
}

export interface PluginEvents {
  on<T = unknown>(name: string, listener: (payload: T) => void | Promise<void>): Promise<Disposable>;
}

export interface PluginLogger {
  debug(message: string, details?: Json): void;
  info(message: string, details?: Json): void;
  warn(message: string, details?: Json): void;
  error(message: string, details?: Json): void;
}

export interface PluginContext {
  readonly pluginId: string;
  readonly commands: PluginCommands;
  readonly search: PluginSearch;
  readonly subInput: PluginSubInput;
  readonly settings: PluginSettings;
  readonly clipboard: PluginClipboard;
  readonly shell: PluginShell;
  readonly screenCapture: PluginScreenCapture;
  readonly cursorColor: PluginCursorColor;
  readonly windowManagement: PluginWindowManagement;
  readonly launcherContext: PluginLauncherContext;
  readonly filesystem: PluginFilesystem;
  readonly native: PluginNative;
  readonly developer: PluginDeveloper;
  readonly events: PluginEvents;
  readonly notifications: {
    show(options: NotificationOptions): Promise<void>;
  };
  readonly logger: PluginLogger;
}

/**
 * Deliberately small compatibility projection installed as both
 * `window.utools` and `window.rubick` while an iHub SDK runtime is active.
 *
 * It is not Electron's uTools preload API. Omitted members are intentional:
 * there is no Node.js, filesystem path, process, remote, arbitrary shell,
 * BrowserWindow, or preload access.
 */
export interface IHubUToolsCompatibilityApi {
  setSubInput(
    onChange: PluginSubInputChangeHandler,
    placeholder?: string,
    isFocus?: boolean,
  ): boolean;
  removeSubInput(): boolean;
  setSubInputValue(value: string): boolean;
  /** Requires `clipboard.write` in plugin.json. */
  copyText(value: string): boolean;
  /** Requires `notifications` in plugin.json. */
  showNotification(body: string): void;
  /** Requires `shell.openExternal` in plugin.json. */
  shellOpenExternal(url: string): void;
  /** Requires `shell.openPath` in plugin.json. */
  shellOpenPath(path: string): void;
  /** Requires `cursorColor` and the trusted iHub confirmation overlay. */
  screenColorPick(callback: (color: CursorColorSample) => void): void;
  getWindowType(): "main";
  isDarkColors(): boolean;
  isWindows(): boolean;
  isMacOS(): boolean;
  isLinux(): boolean;
}

declare global {
  interface Window {
    /** Present only after `bootstrapPlugin` starts in an iHub plugin page. */
    utools?: IHubUToolsCompatibilityApi;
    /** Exact alias of `window.utools`, for the legacy dTools/Rubick name. */
    rubick?: IHubUToolsCompatibilityApi;
  }
}

export interface BootstrapOptions {
  bridge?: HostBridge;
  onError?: (error: unknown) => void;
}

export interface DevelopmentBridge extends HostBridge {
  emit<T = unknown>(event: string, payload: T): Promise<void>;
}
