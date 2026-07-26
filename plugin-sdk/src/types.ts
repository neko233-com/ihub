/** JSON values are the only values that cross the plugin-host IPC boundary. */
export type JsonPrimitive = string | number | boolean | null;
export type Json = JsonPrimitive | Json[] | { [key: string]: Json };

export type PluginTarget =
  | "windows-x86_64"
  | "windows-aarch64"
  | "darwin-x86_64"
  | "darwin-aarch64";

export interface PluginManifest {
  schemaVersion: 1;
  id: string;
  name: string;
  version: string;
  description?: string;
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
  };
  process?: {
    spawn?: boolean;
    allow?: string[];
  };
  shell?: {
    openExternal?: boolean;
    openPath?: boolean;
  };
  globalShortcut?: boolean;
  notifications?: boolean;
  nativeApi?: boolean;
}

export interface PluginContributions {
  commands?: CommandDefinition[];
  searchProviders?: SearchProviderDefinition[];
  settings?: PluginSettingDefinition[];
  quickActions?: QuickActionDefinition[];
}

export interface CommandDefinition {
  id: string;
  title: string;
  subtitle?: string;
  keywords?: string[];
  icon?: string;
  shortcut?: string;
}

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

export type Unlisten = () => void | Promise<void>;

/**
 * A bridge is injected by iHub in production. The SDK also accepts a bridge
 * explicitly, which keeps plugins testable and makes browser-only previews
 * possible without a desktop host.
 */
export interface HostBridge {
  call<T = unknown>(request: HostRequest): Promise<T>;
  listen<T = unknown>(event: string, listener: (payload: T) => void | Promise<void>): Promise<Unlisten>;
}

export interface InjectedHostApi {
  call<T = unknown>(request: HostRequest): Promise<T>;
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

export interface SpawnOptions {
  command: string;
  args?: string[];
  cwd?: string;
  env?: Record<string, string>;
  input?: string;
  timeoutMs?: number;
}

export interface SpawnResult {
  code: number | null;
  stdout: string;
  stderr: string;
  timedOut: boolean;
}

export interface PluginCommands {
  register(definition: CommandDefinition, handler: CommandHandler): Promise<Disposable>;
  execute(commandId: string, input?: Json): Promise<void>;
}

export interface PluginSearch {
  register(definition: SearchProviderDefinition, handler: SearchHandler): Promise<Disposable>;
}

export interface PluginSettings {
  get<T extends Json = Json>(key: string, fallback?: T): Promise<T>;
  set(key: string, value: Json): Promise<void>;
}

export interface PluginClipboard {
  readText(): Promise<string>;
  writeText(value: string): Promise<void>;
}

export interface PluginShell {
  openExternal(url: string): Promise<void>;
  openPath(path: string): Promise<void>;
}

export interface PluginProcess {
  spawn(options: SpawnOptions): Promise<SpawnResult>;
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
  readonly settings: PluginSettings;
  readonly clipboard: PluginClipboard;
  readonly shell: PluginShell;
  readonly process: PluginProcess;
  readonly events: PluginEvents;
  readonly notifications: {
    show(options: NotificationOptions): Promise<void>;
  };
  readonly logger: PluginLogger;
}

export interface BootstrapOptions {
  bridge?: HostBridge;
  onError?: (error: unknown) => void;
}

export interface DevelopmentBridge extends HostBridge {
  emit<T = unknown>(event: string, payload: T): Promise<void>;
}
