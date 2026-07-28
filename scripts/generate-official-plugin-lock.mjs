#!/usr/bin/env node

/**
 * Regenerate immutable official-plugin lock records from local annotated tags.
 *
 * The script reads Git blobs, never working-tree bytes. It is intended for the
 * release maintainer after every plugin repository has a reviewed commit and
 * immutable version tag. `verify-official-plugin-lock.mjs --remote` remains the
 * final gate before the parent registry is pushed.
 */
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, posix, resolve } from "node:path";
import { fileURLToPath } from "node:url";

if (process.argv.length !== 2) {
  throw new Error("Usage: node scripts/generate-official-plugin-lock.mjs");
}

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const registry = readJson(join(root, "plugins", "registry.json"));
const mappings = readJson(
  join(root, "plugins", "official", "repositories.json"),
);
const lockPath = join(root, "plugins", "registry.lock.json");
const previousLock = readJson(lockPath);
const previousById = new Map(
  previousLock.packages.map((entry) => [entry.id, entry]),
);
const mappingsById = uniqueById(mappings.repositories, "repository mappings");

const releaseNotes = {
  "ihub-plugin-ocr":
    "Published v0.2.1. Launcher file/image handoff contains bounded metadata only; the user must still select a PNG/JPG/JPEG grant before the hash-locked Windows OCR worker runs. Cross-host release checks validate the PE statically, while Windows also rebuilds and executes its RPC probes. No network or background scan is available.",
  "ihub-plugin-translate":
    "Published v1.1.0. Explicit launcher text is consumed once and only prefilled; translation occurs only after the user supplies an HTTPS LibreTranslate-compatible endpoint and clicks Translate. The optional API key remains session-only.",
  "ihub-plugin-colorpick":
    "Published v1.1.0. Each native cursor-color sample requires a visible click and separate iHub confirmation, returns only HEX/RGB, and exposes no coordinates, screenshot, recording, or background polling.",
  "ihub-plugin-image-tools":
    "Published v1.1.0. Launcher file/image handoff contains metadata only; pixels are processed locally only after explicit file selection or drop. The plugin has no network, arbitrary filesystem, clipboard, or native capability.",
  "ihub-plugin-text-tools":
    "Published v1.1.0 with a vendored @ihub/plugin-sdk 0.2.0 snapshot. Explicit launcher text is consumed once and only prefilled for an offline operation; it is not transformed, persisted, or uploaded automatically.",
  "ihub-plugin-batch-rename":
    "Published v1.1.0. Launcher file handoff contains metadata only; rename preview and apply still require an owner-scoped user-selected directory grant and the exact one-shot native preview token.",
  "ihub-plugin-window-manager":
    "Published v1.0.2 with a vendored @ihub/plugin-sdk 0.2.0 snapshot for standalone builds. Its only capability is four fixed layout actions applied to iHub's own launcher; it cannot inspect or control other applications.",
  "ihub-plugin-pdf-tools":
    "Published v0.1.0. PDF merge, split, reorder, delete, and rotation run in the current WebView against explicitly selected files with bounded file/page limits; there is no host, network, or native capability.",
  "ihub-plugin-archive-tools":
    "Published v0.1.0. ZIP creation and selective extraction run in the current WebView with zip-slip, size, ratio, encryption, ZIP64, and entry-count preflight limits; downloads remain explicit.",
  "ihub-plugin-web-actions":
    "Published v0.1.0. URL normalization is local and only HTTP(S) targets are accepted; shell.openExternal is called solely after an explicit button click. No fetch, automatic navigation, or native worker is present.",
};

const packages = registry.packages.map((registryPackage) => {
  const id = registryPackage.id;
  const mapping = mappingsById.get(id);
  if (!mapping) {
    throw new Error(`${id} is missing from official/repositories.json.`);
  }
  if (canonicalUrl(mapping.url) !== canonicalUrl(registryPackage.source.url)) {
    throw new Error(`${id} repository URLs disagree.`);
  }
  const requestedRef = registryPackage.channels?.stable;
  if (
    !requestedRef ||
    requestedRef !== registryPackage.source.defaultRef
  ) {
    throw new Error(`${id} stable and default refs disagree.`);
  }

  const repositoryPath = resolve(root, mapping.path);
  const topLevel = gitText(repositoryPath, ["rev-parse", "--show-toplevel"]);
  if (normalizedPath(topLevel) !== normalizedPath(repositoryPath)) {
    throw new Error(`${id} is not an independent Git repository.`);
  }
  const origin = gitText(repositoryPath, ["remote", "get-url", "origin"]);
  if (canonicalUrl(origin) !== canonicalUrl(mapping.url)) {
    throw new Error(`${id} origin does not match ${mapping.url}.`);
  }
  const commit = gitText(repositoryPath, [
    "rev-parse",
    `${requestedRef}^{commit}`,
  ]);
  const manifestBlob = gitBlob(repositoryPath, commit, "plugin.json");
  const manifest = JSON.parse(manifestBlob.toString("utf8"));
  if (manifest.id !== id) {
    throw new Error(`${id} tag contains manifest id ${manifest.id}.`);
  }
  if (requestedRef !== `v${manifest.version}`) {
    throw new Error(`${id} ${requestedRef} contains version ${manifest.version}.`);
  }

  const artifactDescriptors = [];
  const frontend = manifest.entry?.frontend;
  if (typeof frontend !== "string" || !safePath(frontend)) {
    throw new Error(`${id} has an unsafe or missing frontend entry.`);
  }
  const frontendDirectory = posix.dirname(frontend.replaceAll("\\", "/"));
  const frontendPaths = gitText(repositoryPath, [
    "ls-tree",
    "-r",
    "--name-only",
    commit,
    "--",
    frontendDirectory,
  ])
    .split(/\r?\n/)
    .filter(Boolean);
  if (!frontendPaths.includes(frontend.replaceAll("\\", "/"))) {
    throw new Error(`${id} tag does not contain ${frontend}.`);
  }
  for (const path of frontendPaths) {
    artifactDescriptors.push({ target: "web", path });
  }
  for (const binary of manifest.backend?.binaries ?? []) {
    if (
      typeof binary?.target !== "string" ||
      typeof binary?.path !== "string" ||
      !safePath(binary.path)
    ) {
      throw new Error(`${id} contains an unsafe backend binary declaration.`);
    }
    artifactDescriptors.push({
      target: binary.target,
      path: binary.path.replaceAll("\\", "/"),
    });
  }

  const seenArtifacts = new Set();
  const artifacts = artifactDescriptors.map((artifact) => {
    if (seenArtifacts.has(artifact.path)) {
      throw new Error(`${id} lists duplicate artifact ${artifact.path}.`);
    }
    seenArtifacts.add(artifact.path);
    return {
      target: artifact.target,
      path: artifact.path,
      sha256: sha256(gitBlob(repositoryPath, commit, artifact.path)),
    };
  });

  const previous = previousById.get(id);
  const note =
    releaseNotes[id] ??
    previous?.note ??
    `Published ${requestedRef}; immutable manifest and packaged artifacts are hash-locked.`;
  return {
    id,
    source: {
      url: canonicalUrl(mapping.url),
      requestedRef,
    },
    resolved: {
      commit,
      version: manifest.version,
    },
    integrity: {
      manifestSha256: sha256(manifestBlob),
      artifacts,
    },
    permissions: manifest.permissions ?? {},
    verified: true,
    note,
  };
});

if (packages.length !== mappingsById.size) {
  throw new Error("Registry and repository mapping IDs differ.");
}

const nextLock = {
  ...previousLock,
  registry: previousLock.registry,
  lockState: "resolved",
  generatedAt: new Date().toISOString(),
  packages,
};
writeFileSync(lockPath, `${JSON.stringify(nextLock, null, 2)}\n`, "utf8");
console.log(
  `Regenerated ${packages.length} official plugin locks from immutable local Git tags.`,
);

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function uniqueById(entries, label) {
  const indexed = new Map();
  for (const entry of entries ?? []) {
    if (!entry?.id || indexed.has(entry.id)) {
      throw new Error(`${label} contains a missing or duplicate id.`);
    }
    indexed.set(entry.id, entry);
  }
  return indexed;
}

function gitText(cwd, argumentsList) {
  return git(cwd, argumentsList).toString("utf8").trim();
}

function gitBlob(cwd, commit, path) {
  return git(cwd, ["show", `${commit}:${path}`]);
}

function git(cwd, argumentsList) {
  try {
    return execFileSync("git", ["-C", cwd, ...argumentsList], {
      encoding: "buffer",
      env: { ...process.env, GIT_TERMINAL_PROMPT: "0" },
      stdio: ["ignore", "pipe", "pipe"],
    });
  } catch (error) {
    const details = Buffer.isBuffer(error.stderr)
      ? error.stderr.toString("utf8").trim()
      : "";
    throw new Error(
      `git ${argumentsList.join(" ")} failed${details ? `: ${details}` : ""}`,
    );
  }
}

function canonicalUrl(value) {
  if (typeof value !== "string" || !value) {
    throw new Error("Repository URL is missing.");
  }
  const normalized = value.replace(/\.git$/i, "").replace(/\/+$/, "");
  if (!/^https:\/\/github\.com\/[a-z0-9_.-]+\/[a-z0-9_.-]+$/i.test(normalized)) {
    throw new Error(`Only canonical HTTPS GitHub repositories are supported: ${value}`);
  }
  return normalized;
}

function safePath(value) {
  return (
    !value.startsWith("/") &&
    !value.startsWith("\\") &&
    !value
      .split(/[\\/]/)
      .some((part) => !part || part === "." || part === "..")
  );
}

function normalizedPath(value) {
  return value.replaceAll("\\", "/").replace(/\/+$/, "").toLowerCase();
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}
