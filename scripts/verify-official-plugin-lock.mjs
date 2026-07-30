#!/usr/bin/env node

/**
 * Validate the immutable Git source records in plugins/registry.lock.json.
 *
 * Local mode is intentionally offline: it reads the exact blobs from the
 * independently checked-out plugin repositories. `--remote` clones each
 * canonical repository into a scoped temporary directory first, which is the
 * mode used by the release workflow. Neither mode checks a mutable working
 * tree unless `--strict-worktree` is explicitly requested.
 */
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const args = new Set(process.argv.slice(2));

if ([...args].some((arg) => !["--remote", "--strict-worktree"].includes(arg))) {
  throw new Error("Usage: node scripts/verify-official-plugin-lock.mjs [--remote] [--strict-worktree]");
}

const remoteMode = args.has("--remote");
const strictWorktree = args.has("--strict-worktree");
if (remoteMode && strictWorktree) {
  throw new Error("--strict-worktree only applies to local plugin checkouts.");
}

const registry = readJson(join(root, "plugins", "registry.json"));
const lock = readJson(join(root, "plugins", "registry.lock.json"));
const repositories = readJson(join(root, "plugins", "official", "repositories.json"));

const registryById = uniqueById(registry.packages, "registry packages");
const lockById = uniqueById(lock.packages, "registry lock packages");
const repositoriesById = uniqueById(repositories.repositories, "official repository mappings");

assertSameIds(registryById, lockById, "registry.json", "registry.lock.json");
assertSameIds(registryById, repositoriesById, "registry.json", "official/repositories.json");

let temporaryRoot;
const failures = [];

try {
  if (remoteMode) {
    temporaryRoot = mkdtempSync(join(tmpdir(), "ihub-official-plugin-lock-"));
  }

  for (const [id, registryPackage] of registryById) {
    try {
      const lockPackage = lockById.get(id);
      const mapping = repositoriesById.get(id);
      validateMetadata(id, registryPackage, lockPackage, mapping);

      const repositoryPath = remoteMode
        ? cloneRepository(id, lockPackage.source.url, temporaryRoot)
        : localRepositoryPath(id, mapping.path);
      verifyRepository(
        id,
        repositoryPath,
        registryPackage,
        lockPackage,
        strictWorktree,
      );
      console.log(`✓ ${id} (${lockPackage.resolved.commit.slice(0, 12)})`);
    } catch (error) {
      failures.push(`${id}: ${messageFor(error)}`);
    }
  }
} finally {
  if (temporaryRoot) {
    // This path was created above with mkdtempSync, so it is never a user
    // checkout. Clean it on success and failure alike.
    rmSync(temporaryRoot, { recursive: true, force: true });
  }
}

if (failures.length > 0) {
  console.error("\nOfficial plugin lock verification failed:");
  for (const failure of failures) {
    console.error(`- ${failure}`);
  }
  process.exitCode = 1;
} else {
  console.log(`\nVerified ${registryById.size} official plugin lock records (${remoteMode ? "remote" : "local"} mode).`);
}

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function uniqueById(entries, label) {
  if (!Array.isArray(entries)) {
    throw new Error(`${label} must be an array.`);
  }
  const indexed = new Map();
  for (const entry of entries) {
    if (!entry || typeof entry.id !== "string" || !entry.id) {
      throw new Error(`${label} contains an entry without an id.`);
    }
    if (indexed.has(entry.id)) {
      throw new Error(`${label} contains duplicate id ${entry.id}.`);
    }
    indexed.set(entry.id, entry);
  }
  return indexed;
}

function assertSameIds(first, second, firstLabel, secondLabel) {
  for (const id of first.keys()) {
    if (!second.has(id)) {
      throw new Error(`${secondLabel} is missing ${id} from ${firstLabel}.`);
    }
  }
  for (const id of second.keys()) {
    if (!first.has(id)) {
      throw new Error(`${secondLabel} contains ${id}, which is absent from ${firstLabel}.`);
    }
  }
}

function validateMetadata(id, registryPackage, lockPackage, mapping) {
  if (!lockPackage || !mapping) {
    throw new Error("missing registry lock or repository mapping.");
  }
  if (registryPackage.source?.type !== "git") {
    throw new Error("registry package source must be type git.");
  }
  if (canonicalUrl(registryPackage.source.url) !== canonicalUrl(lockPackage.source?.url)) {
    throw new Error("registry and lock source URLs differ.");
  }
  if (canonicalUrl(mapping.url) !== canonicalUrl(lockPackage.source?.url)) {
    throw new Error("repository mapping and lock source URLs differ.");
  }
  if (registryPackage.source.defaultRef !== lockPackage.source.requestedRef) {
    throw new Error("registry defaultRef and lock requestedRef differ.");
  }
  if (registryPackage.channels?.stable !== lockPackage.source.requestedRef) {
    throw new Error("registry stable channel and lock requestedRef differ.");
  }
  if (!/^[0-9a-f]{40}$/i.test(lockPackage.resolved?.commit ?? "")) {
    throw new Error("lock resolved.commit must be a full Git commit SHA.");
  }
  if (!/^[0-9a-f]{64}$/i.test(lockPackage.integrity?.manifestSha256 ?? "")) {
    throw new Error("lock manifestSha256 must be a SHA-256 digest.");
  }
  if (!Array.isArray(lockPackage.integrity?.artifacts) || lockPackage.integrity.artifacts.length === 0) {
    throw new Error("lock must list at least one immutable artifact.");
  }
  for (const artifact of lockPackage.integrity.artifacts) {
    if (typeof artifact.path !== "string" || !isSafeRepositoryPath(artifact.path)) {
      throw new Error("lock contains an unsafe artifact path.");
    }
    if (!/^[0-9a-f]{64}$/i.test(artifact.sha256 ?? "")) {
      throw new Error(`artifact ${artifact.path} has an invalid SHA-256 digest.`);
    }
  }
  if (mapping.path !== `plugins/official/${id}`) {
    throw new Error(`repository mapping path must be plugins/official/${id}.`);
  }
}

function localRepositoryPath(id, mappedPath) {
  const path = resolve(root, mappedPath);
  if (!existsSync(path)) {
    throw new Error(`local checkout is missing at ${mappedPath}; use --remote for a network verification.`);
  }
  const topLevel = gitText(path, ["rev-parse", "--show-toplevel"]);
  if (normalizePath(topLevel) !== normalizePath(path)) {
    throw new Error("is not an independent Git worktree (it resolves to a parent repository).");
  }
  return path;
}

function cloneRepository(id, url, temporaryRoot) {
  const destination = join(temporaryRoot, id);
  runGit(root, ["clone", "--quiet", "--no-checkout", `${canonicalUrl(url)}.git`, destination]);
  return destination;
}

function verifyRepository(
  id,
  repositoryPath,
  registryPackage,
  lockPackage,
  requireCleanWorktree,
) {
  const remote = canonicalUrl(gitText(repositoryPath, ["remote", "get-url", "origin"]));
  if (remote !== canonicalUrl(lockPackage.source.url)) {
    throw new Error(`origin ${remote} does not match the locked source.`);
  }

  const requestedCommit = gitText(repositoryPath, [
    "rev-parse",
    `${lockPackage.source.requestedRef}^{commit}`,
  ]);
  if (requestedCommit !== lockPackage.resolved.commit) {
    throw new Error(`requested ref ${lockPackage.source.requestedRef} resolves to ${requestedCommit}, not the locked commit.`);
  }

  gitText(repositoryPath, ["cat-file", "-e", `${lockPackage.resolved.commit}^{commit}`]);
  const manifestBlob = readGitBlob(
    repositoryPath,
    lockPackage.resolved.commit,
    "plugin.json",
  );
  verifyHash("plugin.json", manifestBlob, lockPackage.integrity.manifestSha256);
  let manifest;
  try {
    manifest = JSON.parse(manifestBlob.toString("utf8"));
  } catch {
    throw new Error("plugin.json is not valid UTF-8 JSON.");
  }
  if (manifest.id !== id) {
    throw new Error(`plugin.json id ${manifest.id ?? "<missing>"} does not match ${id}.`);
  }
  if (manifest.version !== lockPackage.resolved.version) {
    throw new Error(
      `plugin.json version ${manifest.version ?? "<missing>"} does not match lock version ${lockPackage.resolved.version ?? "<missing>"}.`,
    );
  }
  if (lockPackage.source.requestedRef !== `v${manifest.version}`) {
    throw new Error(
      `stable ref ${lockPackage.source.requestedRef} does not match plugin version v${manifest.version}.`,
    );
  }
  if (
    canonicalJson(manifest.permissions ?? {}) !==
    canonicalJson(lockPackage.permissions ?? {})
  ) {
    throw new Error("lock permissions do not exactly match plugin.json.");
  }
  const expectedCapabilities = capabilitiesFromPermissions(
    manifest.permissions ?? {},
  );
  const registryCapabilities = [...(registryPackage.capabilities ?? [])].sort();
  if (new Set(registryCapabilities).size !== registryCapabilities.length) {
    throw new Error("registry capabilities contain duplicates.");
  }
  if (canonicalJson(registryCapabilities) !== canonicalJson(expectedCapabilities)) {
    throw new Error(
      `registry capabilities ${JSON.stringify(registryCapabilities)} do not match plugin permissions ${JSON.stringify(expectedCapabilities)}.`,
    );
  }
  for (const artifact of lockPackage.integrity.artifacts) {
    verifyBlobHash(repositoryPath, lockPackage.resolved.commit, artifact.path, artifact.sha256);
  }

  if (requireCleanWorktree) {
    const head = gitText(repositoryPath, ["rev-parse", "HEAD"]);
    if (head !== lockPackage.resolved.commit) {
      throw new Error(`working tree HEAD ${head} does not equal the locked commit.`);
    }
    const changes = gitText(repositoryPath, ["status", "--porcelain", "--untracked-files=all"]);
    if (changes) {
      throw new Error("working tree has local changes.");
    }
  }
}

function verifyBlobHash(repositoryPath, commit, relativePath, expectedHash) {
  const content = readGitBlob(repositoryPath, commit, relativePath);
  verifyHash(relativePath, content, expectedHash);
}

function readGitBlob(repositoryPath, commit, relativePath) {
  return runGit(repositoryPath, ["show", `${commit}:${relativePath}`]);
}

function verifyHash(relativePath, content, expectedHash) {
  const actualHash = sha256(content);
  if (actualHash !== expectedHash.toLowerCase()) {
    throw new Error(`${relativePath} SHA-256 is ${actualHash}, not the locked digest.`);
  }
}

function capabilitiesFromPermissions(permissions) {
  const capabilities = [];
  for (const [permission, value] of Object.entries(permissions)) {
    if (value === true) {
      capabilities.push(permission);
      continue;
    }
    if (!value || typeof value !== "object" || Array.isArray(value)) {
      continue;
    }
    if (permission === "network") {
      capabilities.push("network");
      continue;
    }
    for (const [scope, enabled] of Object.entries(value)) {
      if (
        enabled === true ||
        (Array.isArray(enabled) && enabled.length > 0)
      ) {
        capabilities.push(`${permission}.${scope}`);
      }
    }
  }
  return capabilities.sort();
}

function canonicalJson(value) {
  if (Array.isArray(value)) {
    return JSON.stringify(value.map((entry) => JSON.parse(canonicalJson(entry))));
  }
  if (value && typeof value === "object") {
    const normalized = {};
    for (const key of Object.keys(value).sort()) {
      normalized[key] = JSON.parse(canonicalJson(value[key]));
    }
    return JSON.stringify(normalized);
  }
  return JSON.stringify(value);
}

function gitText(cwd, args) {
  return runGit(cwd, args).toString("utf8").trim();
}

function runGit(cwd, args) {
  try {
    return execFileSync("git", ["-C", cwd, ...args], {
      encoding: "buffer",
      env: { ...process.env, GIT_TERMINAL_PROMPT: "0" },
      stdio: ["ignore", "pipe", "pipe"],
      windowsHide: true,
    });
  } catch (error) {
    const details = Buffer.isBuffer(error.stderr) ? error.stderr.toString("utf8").trim() : "";
    throw new Error(`git ${args.join(" ")} failed${details ? `: ${details}` : ""}`);
  }
}

function canonicalUrl(value) {
  if (typeof value !== "string" || !value) {
    throw new Error("source URL is missing.");
  }
  return value.replace(/\.git$/i, "").replace(/\/$/, "");
}

function isSafeRepositoryPath(value) {
  return !value.startsWith("/")
    && !value.startsWith("\\")
    && !value.split(/[\\/]/).some((part) => !part || part === "." || part === "..");
}

function normalizePath(path) {
  return path.replace(/\\/g, "/").replace(/\/$/, "").toLowerCase();
}

function sha256(value) {
  return createHash("sha256").update(value).digest("hex");
}

function messageFor(error) {
  return error instanceof Error ? error.message : String(error);
}
