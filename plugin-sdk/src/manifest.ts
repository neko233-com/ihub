import type { PluginManifest } from "./types.js";

export const MANIFEST_SCHEMA_VERSION = 1;

export interface ManifestIssue {
  path: string;
  message: string;
}

export interface ManifestValidationResult {
  valid: boolean;
  issues: ManifestIssue[];
}

const pluginIdPattern = /^[a-z0-9][a-z0-9-]{1,62}$/;
const semverPattern = /^v?(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?$/;
const relativePathPattern = /^(?![\\/])(?!.*(?:^|[\\/])\.\.(?:[\\/]|$)).+$/;
const artworkControlCharacterPattern = /[\u0000-\u001f\u007f-\u009f]/;
const windowsDeviceNamePattern = /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])(?:\..*)?$/i;
const targets = new Set(["windows-x86_64", "windows-aarch64", "darwin-x86_64", "darwin-aarch64"]);
const minNativeCommandTimeoutMs = 1_000;
const maxNativeCommandTimeoutMs = 30 * 60 * 1_000;
const maxManifestCommands = 64;
const maxArtworkCandidates = 32;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function stringAt(value: Record<string, unknown>, key: string, path: string, issues: ManifestIssue[]): string | undefined {
  const result = value[key];
  if (typeof result !== "string" || result.length === 0) {
    issues.push({ path: `${path}.${key}`, message: "must be a non-empty string" });
    return undefined;
  }
  return result;
}

function checkRelativePath(value: unknown, path: string, issues: ManifestIssue[]): void {
  if (typeof value !== "string" || !relativePathPattern.test(value)) {
    issues.push({ path, message: "must be a package-relative path and may not escape the package" });
  }
}

function normalizedArtworkCandidate(value: unknown): string | undefined {
  if (
    typeof value !== "string" ||
    value.length === 0 ||
    value.includes(":") ||
    artworkControlCharacterPattern.test(value) ||
    value.startsWith("/") ||
    value.startsWith("\\")
  ) {
    return undefined;
  }

  const components = value.split(/[\\/]/);
  if (
    components.some(
      (component) =>
        component.length === 0 ||
        component === "." ||
        component === ".." ||
        component.endsWith(".") ||
        component.endsWith(" ") ||
        windowsDeviceNamePattern.test(component),
    )
  ) {
    return undefined;
  }

  return components.join("/");
}

function checkArtworkPath(
  value: unknown,
  path: string,
  issues: ManifestIssue[],
  artworkCandidates: Set<string>,
): void {
  if (typeof value === "string") {
    artworkCandidates.add(value.replaceAll("\\", "/"));
  }
  if (normalizedArtworkCandidate(value) === undefined) {
    issues.push({
      path,
      message:
        "must be a safe package-relative artwork path without control characters, empty/dot components, Windows device names, colons, or trailing dots/spaces",
    });
  }
}

/**
 * A fast developer-facing validation layer. Release tooling should additionally
 * validate the same document against manifest.schema.json with a full JSON
 * Schema validator.
 */
export function validateManifest(value: unknown): ManifestValidationResult {
  const issues: ManifestIssue[] = [];
  const artworkCandidates = new Set<string>();
  if (!isRecord(value)) {
    return { valid: false, issues: [{ path: "$", message: "must be an object" }] };
  }

  if (value.schemaVersion !== MANIFEST_SCHEMA_VERSION) {
    issues.push({ path: "$.schemaVersion", message: `must equal ${MANIFEST_SCHEMA_VERSION}` });
  }

  const id = stringAt(value, "id", "$", issues);
  if (id && !pluginIdPattern.test(id)) {
    issues.push({ path: "$.id", message: "must be lowercase kebab-case, 2–63 characters" });
  }

  stringAt(value, "name", "$", issues);
  const version = stringAt(value, "version", "$", issues);
  if (version && !semverPattern.test(version)) {
    issues.push({ path: "$.version", message: "must be a semantic version" });
  }
  if (value.icon !== undefined && value.logo !== undefined) {
    issues.push({ path: "$", message: "must declare only one of icon or logo" });
  }
  if (value.icon !== undefined) {
    checkArtworkPath(value.icon, "$.icon", issues, artworkCandidates);
  }
  if (value.logo !== undefined) {
    checkArtworkPath(value.logo, "$.logo", issues, artworkCandidates);
  }

  const engines = value.engines;
  if (!isRecord(engines)) {
    issues.push({ path: "$.engines", message: "must be an object with ihub and api ranges" });
  } else {
    stringAt(engines, "ihub", "$.engines", issues);
    stringAt(engines, "api", "$.engines", issues);
  }

  const entry = value.entry;
  if (!isRecord(entry)) {
    issues.push({ path: "$.entry", message: "must be an object with a frontend path" });
  } else {
    checkRelativePath(entry.frontend, "$.entry.frontend", issues);
  }

  if (!isRecord(value.permissions)) {
    issues.push({ path: "$.permissions", message: "must be an object (use {} when no host capability is needed)" });
  } else if (value.permissions.launcherContext !== undefined) {
    const launcherContext = value.permissions.launcherContext;
    if (!isRecord(launcherContext)) {
      issues.push({ path: "$.permissions.launcherContext", message: "must be an object when declared" });
    } else {
      for (const [key, declared] of Object.entries(launcherContext)) {
        if (!(["text", "files", "image"] as const).includes(key as "text" | "files" | "image")) {
          issues.push({ path: `$.permissions.launcherContext.${key}`, message: "is not supported" });
        } else if (typeof declared !== "boolean") {
          issues.push({ path: `$.permissions.launcherContext.${key}`, message: "must be a boolean" });
        }
      }
    }
  }

  if (value.backend !== undefined) {
    const backend = value.backend;
    if (!isRecord(backend)) {
      issues.push({ path: "$.backend", message: "must be an object" });
    } else {
      if (backend.protocol !== "jsonl-rpc-v1") {
        issues.push({ path: "$.backend.protocol", message: "must be jsonl-rpc-v1" });
      }
      if (!Array.isArray(backend.binaries) || backend.binaries.length === 0) {
        issues.push({ path: "$.backend.binaries", message: "must contain at least one platform binary" });
      } else {
        const seenTargets = new Set<string>();
        backend.binaries.forEach((binary, index) => {
          const path = `$.backend.binaries[${index}]`;
          if (!isRecord(binary)) {
            issues.push({ path, message: "must be an object" });
            return;
          }
          if (typeof binary.target !== "string" || !targets.has(binary.target)) {
            issues.push({ path: `${path}.target`, message: "must be a supported Windows or macOS target" });
          } else if (seenTargets.has(binary.target)) {
            issues.push({ path: `${path}.target`, message: "must be unique" });
          } else {
            seenTargets.add(binary.target);
          }
          checkRelativePath(binary.path, `${path}.path`, issues);
        });
      }
    }
  }

  const contributions = value.contributes;
  if (contributions !== undefined && !isRecord(contributions)) {
    issues.push({ path: "$.contributes", message: "must be an object" });
  } else if (isRecord(contributions)) {
    for (const [key, value] of Object.entries(contributions)) {
      if (["commands", "searchProviders", "settings", "quickActions"].includes(key) && !Array.isArray(value)) {
        issues.push({ path: `$.contributes.${key}`, message: "must be an array" });
      }
    }

    if (Array.isArray(contributions.commands)) {
      if (contributions.commands.length > maxManifestCommands) {
        issues.push({
          path: "$.contributes.commands",
          message: `must contain at most ${maxManifestCommands} commands`,
        });
      }
      contributions.commands.forEach((command, index) => {
        const path = `$.contributes.commands[${index}]`;
        if (!isRecord(command)) {
          issues.push({ path, message: "must be an object" });
          return;
        }
        if (command.icon !== undefined) {
          checkArtworkPath(command.icon, `${path}.icon`, issues, artworkCandidates);
        }

        if (command.run === undefined) {
          return;
        }
        const runPath = `${path}.run`;
        if (!isRecord(command.run)) {
          issues.push({ path: runPath, message: "must be an object" });
          return;
        }

        for (const key of Object.keys(command.run)) {
          if (key !== "timeoutMs") {
            issues.push({ path: `${runPath}.${key}`, message: "is not supported" });
          }
        }

        const timeoutMs = command.run.timeoutMs;
        const validTimeout =
          typeof timeoutMs === "number" &&
          Number.isInteger(timeoutMs) &&
          timeoutMs >= minNativeCommandTimeoutMs &&
          timeoutMs <= maxNativeCommandTimeoutMs;
        if (!validTimeout) {
          issues.push({
            path: `${runPath}.timeoutMs`,
            message: `must be an integer between ${minNativeCommandTimeoutMs} and ${maxNativeCommandTimeoutMs} milliseconds`,
          });
        }
        if (command.execution !== "native") {
          issues.push({ path: `${path}.execution`, message: "must be native when run.timeoutMs is declared" });
        }
      });
    }
  }

  if (artworkCandidates.size > maxArtworkCandidates) {
    issues.push({
      path: "$",
      message: `must reference at most ${maxArtworkCandidates} distinct artwork paths`,
    });
  }

  return { valid: issues.length === 0, issues };
}

export function assertValidManifest(value: unknown): asserts value is PluginManifest {
  const result = validateManifest(value);
  if (!result.valid) {
    const details = result.issues.map((issue) => `${issue.path}: ${issue.message}`).join("; ");
    throw new Error(`Invalid iHub plugin manifest: ${details}`);
  }
}
