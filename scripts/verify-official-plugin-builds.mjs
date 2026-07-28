#!/usr/bin/env node

/**
 * Rebuild every package-based official plugin from its independent checkout.
 * Static packages are still checked for their declared frontend entry.
 *
 * Run the immutable lock verifier before this script. Reproducible web builds
 * must leave every tracked artifact equal to the release tag. The Windows OCR
 * worker is also rebuilt and exercised in a disposable checkout because
 * different reviewed MSVC/SDK versions need not emit the same PE bytes.
 */
import { spawnSync } from "node:child_process";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, posix, resolve } from "node:path";
import { fileURLToPath } from "node:url";

if (process.argv.length !== 2) {
  throw new Error("Usage: node scripts/verify-official-plugin-builds.mjs");
}

const root = resolve(fileURLToPath(new URL("..", import.meta.url)));
const mappings = JSON.parse(
  readFileSync(
    join(root, "plugins", "official", "repositories.json"),
    "utf8",
  ),
).repositories;

for (const mapping of mappings) {
  const repositoryPath = resolve(root, mapping.path);
  const manifest = JSON.parse(
    readFileSync(join(repositoryPath, "plugin.json"), "utf8"),
  );
  const frontendPath = join(repositoryPath, manifest.entry.frontend);
  if (!existsSync(frontendPath)) {
    throw new Error(`${mapping.id} is missing ${manifest.entry.frontend}.`);
  }

  const packagePath = join(repositoryPath, "package.json");
  if (!existsSync(packagePath)) {
    console.log(`✓ ${mapping.id} (static package)`);
    continue;
  }
  const packageJson = JSON.parse(readFileSync(packagePath, "utf8"));
  const hasLock = existsSync(join(repositoryPath, "pnpm-lock.yaml"));

  let verificationScope = "reproducible build";
  if (mapping.id === "ihub-plugin-ocr" && process.platform === "win32") {
    verifyWindowsOcrInDisposableCheckout(repositoryPath, manifest, hasLock);
    verificationScope =
      "isolated Windows rebuild/runtime; locked release payload preserved";
  } else {
    runPnpm(repositoryPath, [
      "install",
      hasLock ? "--frozen-lockfile" : "--lockfile=false",
    ]);
  }

  if (
    mapping.id === "ihub-plugin-ocr" &&
    process.platform !== "win32"
  ) {
    // The locked OCR package deliberately ships one Windows-only native
    // worker. Non-Windows matrix jobs rebuild its frontend and run the
    // package's static distribution checks; the Windows matrix job also
    // rebuilds and executes the worker's JSON-RPC verification.
    runPnpm(repositoryPath, ["run", "build:frontend"]);
    run(repositoryPath, "node", ["scripts/verify-dist.mjs"]);
    verificationScope =
      "reproducible frontend and static package; Windows runtime deferred";
  } else if (
    mapping.id === "ihub-plugin-ocr" &&
    process.platform === "win32"
  ) {
    // The isolated verifier above already exercised both the locked payload
    // and a fresh compiler output without changing the release checkout.
  } else if (packageJson.scripts?.verify) {
    runPnpm(repositoryPath, ["run", "verify"]);
  } else if (packageJson.scripts?.build) {
    runPnpm(repositoryPath, ["run", "build"]);
  } else {
    throw new Error(`${mapping.id} has package.json but no build or verify script.`);
  }

  const changes = capture(repositoryPath, "git", [
    "status",
    "--porcelain=v1",
    "--untracked-files=all",
  ]);
  if (changes) {
    throw new Error(
      `${mapping.id} rebuild changed its release checkout:\n${changes}`,
    );
  }
  console.log(`✓ ${mapping.id} (${verificationScope})`);
}

console.log(
  `Verified build and locked-distribution checks for ${mappings.length} official plugins.`,
);

function verifyWindowsOcrInDisposableCheckout(
  repositoryPath,
  manifest,
  hasLock,
) {
  const temporaryRoot = mkdtempSync(join(tmpdir(), "ihub-ocr-verify-"));
  const checkoutPath = join(temporaryRoot, "checkout");
  try {
    run(temporaryRoot, "git", [
      "clone",
      "--quiet",
      "--no-hardlinks",
      "--branch",
      `v${manifest.version}`,
      "--single-branch",
      "--",
      repositoryPath,
      checkoutPath,
    ]);
    runPnpm(checkoutPath, [
      "install",
      hasLock ? "--frozen-lockfile" : "--lockfile=false",
    ]);

    // First execute the immutable, hash-locked release payload. Then rebuild
    // from source and execute that compiler output as a separate check.
    run(checkoutPath, "node", ["scripts/verify-dist.mjs"]);
    runPnpm(checkoutPath, ["run", "verify"]);

    const frontendDirectory = posix.dirname(
      manifest.entry.frontend.replaceAll("\\", "/"),
    );
    const expectedChanges = new Set([
      ...capture(checkoutPath, "git", [
        "ls-files",
        "--",
        frontendDirectory,
      ])
        .split(/\r?\n/)
        .filter(Boolean),
      ...(manifest.backend?.binaries ?? []).map((binary) =>
        binary.path.replaceAll("\\", "/")
      ),
    ]);
    const changedPaths = capture(checkoutPath, "git", [
      "diff",
      "--name-only",
      "--no-renames",
    ])
      .split(/\r?\n/)
      .filter(Boolean);
    const unexpectedChanges = changedPaths.filter(
      (path) => !expectedChanges.has(path),
    );
    const untrackedPaths = capture(checkoutPath, "git", [
      "ls-files",
      "--others",
      "--exclude-standard",
    ])
      .split(/\r?\n/)
      .filter(Boolean);
    if (unexpectedChanges.length || untrackedPaths.length) {
      throw new Error(
        [
          "ihub-plugin-ocr rebuild changed files outside its declared artifacts.",
          ...unexpectedChanges.map((path) => `modified: ${path}`),
          ...untrackedPaths.map((path) => `untracked: ${path}`),
        ].join("\n"),
      );
    }
  } finally {
    rmSync(temporaryRoot, {
      recursive: true,
      force: true,
      maxRetries: 3,
      retryDelay: 100,
    });
  }
}

function run(cwd, command, argumentsList) {
  const result = spawnSync(command, argumentsList, {
    cwd,
    stdio: "inherit",
    windowsHide: true,
  });
  if (result.status !== 0) {
    const detail = result.error ? `: ${result.error.message}` : "";
    throw new Error(
      `${command} ${argumentsList.join(" ")} failed in ${cwd} (${result.status})${detail}.`,
    );
  }
}

function runPnpm(cwd, argumentsList) {
  if (process.platform !== "win32") {
    run(cwd, "corepack", ["pnpm", "--ignore-workspace", ...argumentsList]);
    return;
  }
  const commandTokens = [
    "corepack",
    "pnpm",
    "--ignore-workspace",
    ...argumentsList,
  ];
  if (
    commandTokens.some(
      (token) => !/^[A-Za-z0-9:=._-]+$/.test(token),
    )
  ) {
    throw new Error("Unsafe token in the fixed Corepack command.");
  }
  run(cwd, process.env.ComSpec || "cmd.exe", [
    "/d",
    "/s",
    "/c",
    commandTokens.join(" "),
  ]);
}

function capture(cwd, command, argumentsList) {
  const result = spawnSync(command, argumentsList, {
    cwd,
    encoding: "utf8",
    stdio: ["ignore", "pipe", "pipe"],
    windowsHide: true,
  });
  if (result.status !== 0) {
    throw new Error(
      `${command} ${argumentsList.join(" ")} failed in ${cwd}: ${result.stderr.trim()}`,
    );
  }
  return result.stdout.trim();
}
