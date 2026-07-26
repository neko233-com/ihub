export type SearchKind = "file" | "folder" | "plugin" | "command";

export interface SearchResult {
  id: string;
  name: string;
  path?: string;
  kind: SearchKind;
  score: number;
  metadata?: string;
  modifiedAt?: string;
  pluginId?: string;
  commandId?: string;
}

export interface IndexStatus {
  phase: "idle" | "scanning" | "ready" | "error";
  indexedFiles: number;
  roots: string[];
  lastIndexedAt?: string;
  message?: string;
}

export interface PluginInfo {
  id: string;
  name: string;
  version: string;
  description?: string;
  source?: string;
  frontendEntry?: string;
  enabled?: boolean;
  hasNativeWorker?: boolean;
  commands?: number | PluginCommandInfo[];
  commandCount?: number;
}

export interface PluginCommandInfo {
  id: string;
  name: string;
  description?: string;
}

/**
 * A host event held until a plugin frontend has finished registering its
 * command and search handlers. The `pluginId` is kept outside of the iframe
 * payload so the host, rather than plugin code, owns event routing.
 */
export interface PluginFrontendEvent {
  id: string;
  pluginId: string;
  name: string;
  payload: unknown;
}

export interface AutostartStatus {
  enabled: boolean;
  supported: boolean;
}

export interface AppHealth {
  version: string;
  platform: string;
  autostart?: boolean;
  updateAvailable?: boolean;
}
