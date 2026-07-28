#!/usr/bin/env node

/**
 * Rebuild every package-based official plugin from its independent checkout.
 * Static packages are still checked for their declared frontend entry.
 *
 * Run the immutable lock verifier before this script. A successful rebuild
 * must leave every tracked artifact byte-for-byte equal to its release tag.
 */
import { spawnSync } from "node:child_process";
import { existsSync, readFileSync } from "node:fs";
import { join, resolve } from "node:path";
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
  runPnpm(repositoryPath, [
    "install",
    hasLock ? "--frozen-lockfile" : "--lockfile=false",
  ]);

  if (
    mapping.id === "ihub-plugin-ocr" &&
    process.platform !== "win32"
  ) {
    runPnpm(repositoryPath, ["run", "build:frontend"]);
    run(repositoryPath, "node", ["scripts/verify-dist.mjs"]);
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
  console.log(`✓ ${mapping.id} (reproducible build)`);
}

console.log(`Verified reproducible builds for ${mappings.length} official plugins.`);

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
    run(cwd, "corepack", ["pnpm", ...argumentsList]);
    return;
  }
  const commandTokens = ["corepack", "pnpm", ...argumentsList];
  if (
    commandTokens.some(
      (token) => !/^[A-Za-z0-9:._-]+$/.test(token),
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
