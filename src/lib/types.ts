export type SearchKind = "file" | "folder" | "application" | "plugin" | "command";

export interface SearchResult {
  id: string;
  name: string;
  path?: string;
  kind: SearchKind;
  /** Host-derived eligibility for an opaque, native-persisted launcher pin. */
  pinEligible?: boolean;
  /** Opaque native shortcut ID when this exact indexed source is already pinned. */
  pinnedShortcutId?: string;
  score: number;
  metadata?: string;
  modifiedAt?: string;
  /** Index-time file length, present only when it is exact in JavaScript. */
  sizeBytes?: number;
  /** Short-lived host authorization for an explicitly pasted filesystem item. */
  openId?: string;
  pluginId?: string;
  commandId?: string;
  /** A host-owned expression carried only into the built-in calculator UI. */
  calculatorExpression?: string;
  /** A parsed launcher value carried only into the built-in time UI. */
  timeInput?: string;
  /** A result produced by a manifest-declared iframe search provider. */
  pluginProviderId?: string;
  /** Opaque native query snapshot used to validate a detached selection. */
  pluginSearchRequestId?: string;
  pluginSearchResultId?: string;
  pluginPayload?: unknown;
}

/** A host-owned pinned file/folder/application. Its ID is opaque: neither the
 * target path nor the native index source ID is returned to the WebView. */
export interface LauncherShortcutView {
  id: string;
  name: string;
  kind: Extract<SearchKind, "file" | "folder" | "application">;
  metadata: string;
  status: "ready" | "unavailable" | string;
}

export interface IndexStatus {
  phase: "idle" | "scanning" | "ready" | "error";
  indexedFiles: number;
  /** Number of in-memory, bounded UTF-8 text documents ready for `content:` searches. */
  contentIndexedFiles?: number;
  /** Approximate bytes held in process memory for the content index; never a disk cache. */
  contentIndexedBytes?: number;
  /** Kept separate from path-index health so filenames remain responsive during body indexing. */
  contentStatus?: "idle" | "indexing" | "ready" | "stale" | string;
  roots: string[];
  lastIndexedAt?: string;
  /** Native watcher health for only the configured local-search roots. */
  watchStatus?: "not-started" | "starting" | "watching" | "degraded" | "unavailable" | "inactive";
  /** Human-readable reason when continuous refresh is degraded or unavailable. */
  watchMessage?: string;
  /** Windows-only NTFS USN health; `available` can also mean a P1d zero-change snapshot reuse, never delta replay. */
  usnStatus?: "not-started" | "probing" | "available" | "degraded" | "fallback" | "inactive" | "unsupported" | string;
  /** Authorised NTFS volumes whose serial number and live USN watermark were queried. */
  usnEligibleVolumes?: number;
  /** Persisted volume checkpoints that remain inside the live journal range. */
  usnCheckpointedVolumes?: number;
  /** Local-only P1a probe/fallback detail, suitable for a diagnostics surface. */
  usnMessage?: string;
  /** Windows P1c read-only MFT initialization or P1d zero-change reuse health; narrow roots intentionally stay on the scoped scanner. */
  mftStatus?: "not-started" | "scanning" | "available" | "degraded" | "fallback" | "inactive" | "unsupported" | string;
  /** Raw MFT records read during a complete explicit-drive-root initialization; not a file count. */
  mftEnumeratedRecords?: number;
  /** USN V2 records considered only inside this initialization window; never a restart checkpoint. */
  mftReplayedUsnRecords?: number;
  /** Safe path projections accepted from the transient MFT data. */
  mftIndexedPaths?: number;
  /** Honest local-only MFT boundary, fallback, or initialization-window replay detail. */
  mftMessage?: string;
  /** Bounded text-index progress or privacy/status detail. */
  contentMessage?: string;
  message?: string;
}

/** One exact folder chosen through the native host picker. The path is for
 * display only; filesystem commands must send the short-lived openId. */
export interface SelectedDirectoryGrant {
  path: string;
  openId: string;
}

export interface PluginInfo {
  id: string;
  name: string;
  version: string;
  description?: string;
  /** Host-validated PNG data URL; plugin package paths never reach browser code. */
  iconSrc?: string;
  source?: string;
  /** Legacy-compatible resolved Git revision also returned by the native host. */
  commit?: string;
  installedAt?: string;
  /** Immutable Git provenance for installs created by current iHub versions. */
  sourceLock?: PluginSourceLock;
  /**
   * True while an explicit local development-link record owns this plugin ID.
   * A stale link may be running its managed snapshot fallback and stays true
   * so Plugin Center can still expose the unlink action.
   */
  isDevelopmentLink?: boolean;
  /** Native-validated health of the explicit local development link. */
  localLinkStatus?: "active" | "stale" | string;
  /** Bounded host diagnostic explaining why a local link is stale. */
  localLinkError?: string;
  /** A stale link is currently executing the validated managed snapshot. */
  usesManagedSnapshotFallback?: boolean;
  /** Last canonical project directory recorded for an explicit local link. */
  localPath?: string;
  frontendEntry?: string;
  enabled?: boolean;
  hasNativeWorker?: boolean;
  /** Manifest-declared release channel. Automatic availability checks only use stable. */
  updateChannel?: "stable" | "beta" | string;
  /** Opt-in for bounded automatic availability checks, never silent native replacement. */
  autoUpdate?: boolean;
  commands?: number | PluginCommandInfo[];
  commandCount?: number;
  /** Manifest-declared uTools MCP tools; a hidden sandbox runtime still has
   * to register each exact handler before an Agent can invoke it. */
  toolCount?: number;
  /** Host-owned plugin-level shortcut-to-command/keyword mappings. */
  globalShortcuts?: PluginGlobalShortcutInfo[];
  /** Metadata declared in the plugin manifest. The iframe must still register
   * a provider before the host sends it a real query. */
  searchProviders?: PluginSearchProviderInfo[];
  /**
   * Read-only, manifest-declared eligibility for a parent-owned, explicit
   * launcher handoff. This is not a browser capability: the native host
   * revalidates every category before it creates a one-shot context token.
   */
  launcherContext?: PluginLauncherContextPermissions;
}

export interface UtoolsToolCatalogEntry {
  pluginId: string;
  pluginName: string;
  name: string;
  description: string;
  inputSchema: Record<string, unknown>;
  outputSchema?: Record<string, unknown>;
  registered: boolean;
}

export interface UtoolsToolInvocationResult {
  requestId: string;
  result: unknown;
}

export interface UtoolsToolProgressEvent {
  requestId: string;
  pluginId: string;
  name: string;
  progress: number;
  total?: number;
  message?: string;
}

export interface AiProviderModel {
  id: string;
  label: string;
  description: string;
}

export interface AiProviderProfile {
  id: string;
  label: string;
  endpoint: string;
  models: AiProviderModel[];
  defaultModel: string;
  hasApiKey: boolean;
  isDefault: boolean;
}

export interface AiProviderTestResult {
  reachable: boolean;
  modelIds: string[];
  message: string;
}

export interface UtoolsAiModel {
  id: string;
  label: string;
  description: string;
  icon: string;
  cost: number;
}

/** Native-validated availability for a first-party plugin that can be linked
 * only from the source checkout trusted by this development installation. */
export interface OfficialWorkspacePluginProject {
  id: string;
  name: string;
  available: boolean;
  localPath?: string;
  detail: string;
}

export interface PluginLauncherContextPermissions {
  text?: boolean;
  files?: boolean;
  image?: boolean;
}

export interface PluginSourceLock {
  source: string;
  requestedRef: string;
  resolvedCommit: string;
  installedAt: string;
  /** Runtime files hashed by the host when this immutable Git snapshot was imported. */
  integrity?: PluginSnapshotIntegrity;
}

export interface PluginSnapshotIntegrity {
  algorithm: "sha256" | string;
  manifestSha256: string;
  frontendAssets: PluginArtifactDigest[];
  /** Absent only on source locks created before standalone artwork was covered. */
  artworkAssets?: PluginArtifactDigest[];
  nativeBinaries: PluginArtifactDigest[];
}

export interface PluginArtifactDigest {
  /** Normalized package-relative path, separated with `/`. */
  path: string;
  sha256: string;
}

/** Immutable comparison returned by the host before a Git plugin is updated. */
export interface PluginUpdateCheck {
  pluginId: string;
  source: string;
  requestedRef: string;
  currentCommit: string;
  latestCommit: string;
  updateAvailable: boolean;
  status: "up-to-date" | "update-available";
  message: string;
}

/** A deliberate exclusion from the bounded automatic update-discovery pass. */
export interface PluginAutomaticUpdateSkip {
  pluginId: string;
  reason: string;
}

/**
 * Read-only background discovery for trusted, stable plugin sources. A report
 * only identifies a reviewed commit; applying it still requires an explicit
 * user confirmation through the normal update command.
 */
export interface PluginAutomaticUpdateReport {
  checkedAt: string;
  checks: PluginUpdateCheck[];
  skipped: PluginAutomaticUpdateSkip[];
}

/** Result of an explicit Git plugin update. The plugin list remains canonical. */
export interface PluginUpdateResult {
  plugin: PluginInfo;
  updated: boolean;
  previousCommit: string;
  currentCommit: string;
}

/** Canonical lifecycle state returned after enabling or disabling a plugin. */
export interface PluginLifecycleUpdate {
  plugin: PluginInfo;
  enabled: boolean;
}

/** A managed Git snapshot was removed. Local development projects never use
 * this result because iHub refuses to delete developer-owned source trees. */
export interface PluginUninstallResult {
  pluginId: string;
  pluginName: string;
  source: string;
}

export interface PluginCommandInfo {
  id: string;
  name: string;
  description?: string;
  /** Host-validated PNG data URL; never the manifest's local artwork path. */
  iconSrc?: string;
  /** Whether activation opens the plugin iframe or starts a declared worker. */
  execution: "frontend" | "native";
  /** Static manifest aliases used by launcher search. */
  keywords?: string[];
  /** Canonical manifest-owned global accelerator. */
  shortcut?: string;
  shortcutRegistration?: "registered" | "blocked" | "unavailable" | "inactive" | string;
  shortcutError?: string;
}

export interface PluginGlobalShortcutInfo {
  id: string;
  shortcut: string;
  commandId?: string;
  keyword?: string;
  registration: "registered" | "blocked" | "unavailable" | "inactive" | string;
  error?: string;
}

export interface PluginGlobalShortcutEvent {
  pluginId: string;
  shortcut: string;
  commandId?: string;
  keyword?: string;
}

/** Bounded outcome returned after iHub waits for a one-shot native plugin command. */
export interface PluginCommandResult {
  pluginId: string;
  commandId: string;
  success: boolean;
  exitCode?: number | null;
  stdout: string;
  stderr: string;
  output?: unknown;
}

/** Only an opaque ID and short expiry cross from the trusted parent to the
 * plugin command event. The context payload remains host-owned until the SDK
 * explicitly consumes it once. */
export interface PluginLauncherContextIssue {
  contextId: string;
  expiresInMs: number;
}

export interface PluginSearchProviderInfo {
  id: string;
  title: string;
  trigger?: string;
  priority?: number;
}

/** A bounded response produced by the native host after it correlates an
 * iframe provider's `search.complete` message. */
export interface PluginSearchResponse {
  requestId: string;
  pluginId: string;
  providerId: string;
  results: PluginSearchProviderResult[];
}

export interface RegisteredPluginSearchProvider {
  pluginId: string;
  providerId: string;
}

export interface PluginSearchProviderResult {
  id: string;
  title: string;
  subtitle?: string;
  score: number;
  payload?: unknown;
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

/** A host-issued, short-lived loopback source for a plugin iframe. The
 * renderer consumes this directly; plugin SDK requests never receive the
 * lease ID. */
export interface PluginFrontendLease {
  leaseId: string;
  url: string;
  origin: string;
  /** Native-projected capability. True only for a visible lease whose
   * validated manifest declares permissions.screenCapture. */
  allowsDisplayCapture: boolean;
  /** Native-projected capability. True only for a visible lease whose
   * validated manifest declares permissions.microphone. */
  allowsMicrophone: boolean;
}

export interface AutostartStatus {
  enabled: boolean;
  supported: boolean;
}

/** The native shell's global launcher hotkey result. Registration remains
 * native-host owned; the renderer receives only the resolved status. */
export interface LauncherHotkeyStatus {
  registration: "primary" | "fallback" | "configured" | "unavailable";
  /** The accelerator actually registered by the host, in canonical form. */
  accelerator?: string;
  /** The requested binding when the host had to register a fallback instead. */
  preferredAccelerator?: string;
  /** The native tray's “Show iHub” action remains usable for recovery. */
  trayShowAvailable: boolean;
}

export interface SuperPanelStatus {
  enabled: boolean;
  listenerRunning: boolean;
  holdMs: number;
  error?: string;
}

export interface SuperPanelEvent {
  contextToken: string;
  physicalX: number;
  physicalY: number;
  expiresInMs: number;
}

export type SuperPanelContext =
  | { kind: "files"; files: ClipboardFile[] }
  | { kind: "image"; image: ClipboardImage }
  | { kind: "text"; text: string }
  | { kind: "empty" };

export interface AppHealth {
  version: string;
  platform: string;
  /** Canonical release target reported by the native host, such as windows-x86_64. */
  hostTarget: string;
  autostart?: boolean;
  /** Native-host-owned launcher hotkey registration; unavailable in old shells. */
  launcherHotkey?: LauncherHotkeyStatus;
  updateAvailable?: boolean;
}

export type HostLogLevel = "debug" | "info" | "warn" | "error";

/** One host-sanitized diagnostic entry. The native logger never projects its
 * file location, plugin detail objects, clipboard payloads, or raw paths. */
export interface HostLogEntry {
  timestamp: string;
  level: HostLogLevel;
  component: string;
  message: string;
}

/** Bounded projection of every retained rotating host log file. */
export interface HostLogSnapshot {
  generatedAt: string;
  entries: HostLogEntry[];
  truncated: boolean;
  totalBytes: number;
  activeFileBytes: number;
  maxFileBytes: number;
  maxFiles: number;
  writeFailures: number;
  lastWriteError?: string;
}

export type ClipboardHistoryItemKind = "text" | "image" | "files";

/** Bounded image metadata only. Pixels stay in native storage until a person
 * explicitly requests a preview or restores the image to the clipboard. */
export interface ClipboardHistoryImageMetadata {
  width: number;
  height: number;
  byteLength: number;
}

/** A display-only projection of a native-private clipboard file reference.
 * Paths and fingerprints are intentionally not returned to the renderer. */
export interface ClipboardHistoryFileMetadata {
  name: string;
  kind: "file" | "folder" | string;
}

/** A local history entry captured after the matching opt-in. Text remains the
 * default mode; image/file data has independent consent and restore actions. */
export interface ClipboardHistoryItem {
  id: string;
  kind: ClipboardHistoryItemKind;
  text: string;
  capturedAt: string;
  pinned: boolean;
  image?: ClipboardHistoryImageMetadata;
  files: ClipboardHistoryFileMetadata[];
}

/** Host-owned, on-device clipboard history state. */
export interface ClipboardHistorySnapshot {
  enabled: boolean;
  imageHistoryEnabled: boolean;
  fileHistoryEnabled: boolean;
  items: ClipboardHistoryItem[];
}

/** Explicit native restore result. No history item is restored implicitly. */
export interface ClipboardHistoryRestoreResult {
  kind: ClipboardHistoryItemKind;
  restoredCount: number;
}

/** A canonical filesystem item explicitly copied by the user. */
export interface ClipboardFile {
  path: string;
  name: string;
  kind: "file" | "folder";
  /** Opaque, bounded native authorization; never a renderer-selected path. */
  openId: string;
}

/** A bounded PNG returned only for an explicit launcher image paste. */
export interface ClipboardImage {
  dataUrl: string;
  name: string;
  mimeType: string;
  width: number;
  height: number;
}
