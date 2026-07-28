#!/usr/bin/env node

import {
  existsSync,
  mkdirSync,
  readFileSync,
  renameSync,
  rmSync,
} from "node:fs";
import { basename, dirname, join, relative, resolve, sep } from "node:path";
import { spawnSync } from "node:child_process";

const argumentsSet = new Set(process.argv.slice(2));
const allowedArguments = new Set(["--locked", "--update", "--update-if-clean"]);
for (const argument of argumentsSet) {
  if (!allowedArguments.has(argument)) {
    throw new Error(`Unknown argument: ${argument}`);
  }
}

const updateModes = ["--update", "--update-if-clean"].filter((argument) =>
  argumentsSet.has(argument),
);
const selectedModes = ["--locked", ...updateModes].filter((argument) =>
  argumentsSet.has(argument),
);
if (selectedModes.length > 1) {
  throw new Error("Use only one of --locked, --update, or --update-if-clean.");
}

const mode = updateModes[0] ?? "--locked";
const continueOnSkip = mode === "--update-if-clean";
const root = resolve(import.meta.dirname, "..");
const officialRoot = join(root, "plugins", "official");
const repositoriesPath = join(officialRoot, "repositories.json");
const registryPath = join(root, "plugins", "registry.json");
const lockPath = join(root, "plugins", "registry.lock.json");

function readJson(path) {
  return JSON.parse(readFileSync(path, "utf8"));
}

function runGit(cwd, commandArguments, options = {}) {
  const result = spawnSync("git", commandArguments, {
    cwd,
    encoding: "utf8",
    stdio: options.capture === false ? "inherit" : "pipe",
    windowsHide: true,
  });
  if (result.status !== 0 && !options.allowFailure) {
    const detail = `${result.stderr ?? ""}${result.stdout ?? ""}`.trim();
    throw new Error(
      `git ${commandArguments.join(" ")} failed in ${cwd}${detail ? `:\n${detail}` : ""}`,
    );
  }
  return {
    ok: result.status === 0,
    stdout: (result.stdout ?? "").trim(),
    stderr: (result.stderr ?? "").trim(),
  };
}

function normalizedRepositoryUrl(value) {
  return value.trim().replace(/\/+$/, "").replace(/\.git$/i, "").toLowerCase();
}

function assertRepositoryUrl(value, id) {
  if (
    typeof value !== "string" ||
    !/^https:\/\/github\.com\/[a-z0-9_.-]+\/[a-z0-9_.-]+(?:\.git)?$/i.test(value)
  ) {
    throw new Error(`${id} must use a canonical HTTPS GitHub repository URL.`);
  }
}

function assertCheckoutPath(id, configuredPath) {
  const expectedRelativePath = `plugins/official/${id}`;
  if (configuredPath !== expectedRelativePath) {
    throw new Error(`${id} path must be ${expectedRelativePath}.`);
  }
  const checkoutPath = resolve(root, configuredPath);
  const relativePath = relative(officialRoot, checkoutPath);
  if (
    relativePath === "" ||
    relativePath.startsWith(`..${sep}`) ||
    relativePath === ".." ||
    basename(checkoutPath) !== id
  ) {
    throw new Error(`${id} resolves outside the official plugin workspace.`);
  }
  return checkoutPath;
}

function skipOrThrow(message) {
  if (continueOnSkip) {
    console.warn(`skip: ${message}`);
    return false;
  }
  throw new Error(message);
}

function ensureExistingCheckout(mapping, checkoutPath, lockedPackage) {
  if (!existsSync(join(checkoutPath, ".git"))) {
    throw new Error(
      `${mapping.id} exists but is not an independent Git checkout: ${checkoutPath}`,
    );
  }

  const origin = runGit(checkoutPath, ["remote", "get-url", "origin"]).stdout;
  if (
    normalizedRepositoryUrl(origin) !== normalizedRepositoryUrl(mapping.url)
  ) {
    throw new Error(
      `${mapping.id} origin mismatch: expected ${mapping.url}, found ${origin}.`,
    );
  }

  if (mode === "--locked") {
    const commit = lockedPackage.resolved.commit;
    const object = runGit(
      checkoutPath,
      ["cat-file", "-e", `${commit}^{commit}`],
      { allowFailure: true },
    );
    if (!object.ok) {
      runGit(
        checkoutPath,
        ["fetch", "--no-tags", "--depth", "1", "origin", lockedPackage.source.requestedRef],
        { capture: false },
      );
      const fetchedCommit = runGit(checkoutPath, [
        "rev-parse",
        "FETCH_HEAD^{commit}",
      ]).stdout;
      if (fetchedCommit !== commit) {
        throw new Error(
          `${mapping.id} ${lockedPackage.source.requestedRef} resolved to ${fetchedCommit}, expected ${commit}.`,
        );
      }
    }
    const head = runGit(checkoutPath, ["rev-parse", "--short", "HEAD"]).stdout;
    console.log(`ready: ${mapping.id} (${head}; locked object available)`);
    return;
  }

  const changes = runGit(checkoutPath, [
    "status",
    "--porcelain=v1",
    "--untracked-files=normal",
  ]).stdout;
  if (changes && !skipOrThrow(`${mapping.id} has local changes; no update was attempted.`)) {
    return;
  }

  const branchResult = runGit(
    checkoutPath,
    ["symbolic-ref", "--quiet", "--short", "HEAD"],
    { allowFailure: true },
  );
  if (
    (!branchResult.ok || branchResult.stdout !== "main") &&
    !skipOrThrow(`${mapping.id} is not on main; no update was attempted.`)
  ) {
    return;
  }

  runGit(checkoutPath, ["fetch", "--prune", "--tags", "origin"], {
    capture: false,
  });
  const counts = runGit(checkoutPath, [
    "rev-list",
    "--left-right",
    "--count",
    "HEAD...origin/main",
  ]).stdout
    .split(/\s+/)
    .map(Number);
  if (counts.length !== 2 || counts.some((value) => !Number.isSafeInteger(value))) {
    throw new Error(`${mapping.id} divergence could not be determined.`);
  }
  const [ahead, behind] = counts;
  if (
    ahead > 0 &&
    !skipOrThrow(
      `${mapping.id} is ${ahead} commit(s) ahead and ${behind} behind origin/main.`,
    )
  ) {
    return;
  }
  if (behind > 0) {
    runGit(checkoutPath, ["merge", "--ff-only", "origin/main"], {
      capture: false,
    });
  }
  console.log(
    behind > 0
      ? `updated: ${mapping.id} (+${behind} commit${behind === 1 ? "" : "s"})`
      : `current: ${mapping.id}`,
  );
}

function cloneCheckout(mapping, checkoutPath, lockedPackage) {
  mkdirSync(dirname(checkoutPath), { recursive: true });
  const temporaryPath = `${checkoutPath}.bootstrap-${process.pid}-${Math.random()
    .toString(16)
    .slice(2)}`;
  const relativeTemporaryPath = relative(officialRoot, temporaryPath);
  if (
    relativeTemporaryPath.startsWith(`..${sep}`) ||
    relativeTemporaryPath === ".."
  ) {
    throw new Error(`Unsafe temporary checkout path: ${temporaryPath}`);
  }

  try {
    if (mode === "--locked") {
      runGit(officialRoot, [
        "clone",
        "--filter=blob:none",
        "--no-checkout",
        mapping.url,
        temporaryPath,
      ], { capture: false });
      configureFreshCheckout(temporaryPath);
      runGit(
        temporaryPath,
        [
          "fetch",
          "--force",
          "--no-tags",
          "--depth",
          "1",
          "origin",
          lockedPackage.source.requestedRef,
        ],
        { capture: false },
      );
      const fetchedCommit = runGit(temporaryPath, [
        "rev-parse",
        "FETCH_HEAD^{commit}",
      ]).stdout;
      if (fetchedCommit !== lockedPackage.resolved.commit) {
        throw new Error(
          `${mapping.id} ${lockedPackage.source.requestedRef} resolved to ${fetchedCommit}, expected ${lockedPackage.resolved.commit}.`,
        );
      }
      runGit(temporaryPath, [
        "checkout",
        "--detach",
        lockedPackage.resolved.commit,
      ], { capture: false });
    } else {
      runGit(
        officialRoot,
        [
          "clone",
          "--branch",
          "main",
          "--single-branch",
          "--no-checkout",
          mapping.url,
          temporaryPath,
        ],
        { capture: false },
      );
      configureFreshCheckout(temporaryPath);
      runGit(temporaryPath, ["checkout", "main"], { capture: false });
    }
    renameSync(temporaryPath, checkoutPath);
    console.log(`cloned: ${mapping.id}`);
  } finally {
    if (existsSync(temporaryPath)) {
      rmSync(temporaryPath, { recursive: true, force: true });
    }
  }
}

function configureFreshCheckout(checkoutPath) {
  // Git for Windows commonly defaults to core.autocrlf=true. Generated web
  // artifacts are committed and rebuilt as LF on every platform, so configure
  // the independent checkout before its first checkout. Otherwise a build can
  // replace an equivalent CRLF worktree file with LF and leave a false-positive
  // dirty status even though `git diff` has no content changes.
  runGit(checkoutPath, ["config", "core.autocrlf", "false"]);
  runGit(checkoutPath, ["config", "core.eol", "lf"]);
}

const repositories = readJson(repositoriesPath).repositories;
const registryPackages = new Map(
  readJson(registryPath).packages.map((entry) => [entry.id, entry]),
);
const lockedPackages = new Map(
  readJson(lockPath).packages.map((entry) => [entry.id, entry]),
);

if (!Array.isArray(repositories) || repositories.length === 0) {
  throw new Error("Official repository mapping is empty.");
}

for (const mapping of repositories) {
  if (!mapping || !/^[a-z0-9][a-z0-9-]{1,62}$/.test(mapping.id)) {
    throw new Error("Official repository mapping contains an invalid id.");
  }
  assertRepositoryUrl(mapping.url, mapping.id);
  const checkoutPath = assertCheckoutPath(mapping.id, mapping.path);
  const registryPackage = registryPackages.get(mapping.id);
  const lockedPackage = lockedPackages.get(mapping.id);
  if (!registryPackage || !lockedPackage) {
    throw new Error(`${mapping.id} is missing from the registry or lock.`);
  }
  if (
    normalizedRepositoryUrl(registryPackage.source.url) !==
      normalizedRepositoryUrl(mapping.url) ||
    normalizedRepositoryUrl(lockedPackage.source.url) !==
      normalizedRepositoryUrl(mapping.url)
  ) {
    throw new Error(`${mapping.id} repository URLs disagree across catalog files.`);
  }
  if (
    registryPackage.channels?.stable !== lockedPackage.source.requestedRef ||
    registryPackage.source.defaultRef !== lockedPackage.source.requestedRef
  ) {
    throw new Error(`${mapping.id} stable refs disagree across catalog files.`);
  }
  if (!/^[0-9a-f]{40}$/.test(lockedPackage.resolved?.commit ?? "")) {
    throw new Error(`${mapping.id} lock is missing a full immutable commit.`);
  }

  if (existsSync(checkoutPath)) {
    ensureExistingCheckout(mapping, checkoutPath, lockedPackage);
  } else {
    cloneCheckout(mapping, checkoutPath, lockedPackage);
  }
}

console.log(
  `Official plugin workspace is ready (${repositories.length} independent repositories; mode ${mode.slice(2)}).`,
);
