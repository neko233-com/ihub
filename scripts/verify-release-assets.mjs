#!/usr/bin/env node
// Verifies the release assets that the Tauri GitHub Action uploaded before a
// draft release is made public. It intentionally verifies relationships that a
// SHA-256 manifest cannot: latest.json must have usable signed update entries
// for every platform iHub promises to ship.

import { existsSync, readFileSync, readdirSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import {
  assertSimpleTag,
  parseBrowserDownloadUrl,
  parseRepository,
} from './normalize-release-updater-json.mjs';

const PLATFORM_INSTALLERS = Object.freeze({
  'windows-x86_64': (version) => `ihub_${version}_windows_x64_setup.exe`,
  'darwin-aarch64': (version) => `ihub_${version}_darwin_aarch64.dmg`,
  'darwin-x86_64': (version) => `ihub_${version}_darwin_x64.dmg`,
});
const ALL_PLATFORM_KEYS = Object.freeze(Object.keys(PLATFORM_INSTALLERS));

function usage() {
  console.log(`Usage: node scripts/verify-release-assets.mjs --input-dir DIR --repository OWNER/REPO --tag vX.Y.Z [--platforms KEY[,KEY...]]

Validates the installer names consumed by iHub bootstrap installers and the
signed updater entries in latest.json. --platforms defaults to every supported
platform. The release is not modified.`);
}

function fail(message) {
  throw new Error(message);
}

function readOption(argumentsList, option) {
  const index = argumentsList.indexOf(option);
  if (index === -1) {
    return undefined;
  }
  const value = argumentsList[index + 1];
  if (!value || value.startsWith('--')) {
    fail(`${option} requires a value.`);
  }
  return value;
}

export function parseOptions(argumentsList) {
  if (argumentsList.includes('--help') || argumentsList.includes('-h')) {
    usage();
    process.exit(0);
  }

  const knownOptions = new Set(['--input-dir', '--repository', '--tag', '--platforms']);
  for (let index = 0; index < argumentsList.length; index += 1) {
    const argument = argumentsList[index];
    if (!knownOptions.has(argument)) {
      fail(`Unknown option: ${argument}`);
    }
    index += 1;
  }

  const inputDirectory = readOption(argumentsList, '--input-dir');
  const repository = readOption(argumentsList, '--repository');
  const tag = readOption(argumentsList, '--tag');
  const platforms = readOption(argumentsList, '--platforms');
  if (!inputDirectory || !repository || !tag) {
    fail('--input-dir, --repository, and --tag are required.');
  }
  parseRepository(repository);
  assertSimpleTag(tag);
  const platformKeys = platforms
    ? [...new Set(platforms.split(',').map((value) => value.trim()).filter(Boolean))]
    : [...ALL_PLATFORM_KEYS];
  if (platformKeys.length === 0) {
    fail('--platforms must contain at least one platform key.');
  }
  for (const platformKey of platformKeys) {
    if (!Object.hasOwn(PLATFORM_INSTALLERS, platformKey)) {
      fail(`Unsupported release platform: ${platformKey}.`);
    }
  }

  return { inputDirectory: resolve(inputDirectory), repository, tag, platformKeys };
}

export function validateLatestJson({ latest, assets, repository, tag, platformKeys = ALL_PLATFORM_KEYS }) {
  if (!latest || typeof latest !== 'object' || Array.isArray(latest)) {
    fail('latest.json must contain an object.');
  }

  const expectedVersion = tag.startsWith('v') ? tag.slice(1) : tag;
  if (typeof latest.version !== 'string' || (latest.version !== expectedVersion && latest.version !== tag)) {
    fail(`latest.json version must equal '${tag}' or '${expectedVersion}'.`);
  }
  if (!latest.platforms || typeof latest.platforms !== 'object' || Array.isArray(latest.platforms)) {
    fail('latest.json must contain a platforms object.');
  }

  for (const platformKey of platformKeys) {
    const platform = latest.platforms[platformKey];
    if (!platform || typeof platform !== 'object' || Array.isArray(platform)) {
      fail(`latest.json is missing platform '${platformKey}'.`);
    }
    if (typeof platform.url !== 'string' || !platform.url.trim()) {
      fail(`latest.json platform '${platformKey}' has no update URL.`);
    }
    if (typeof platform.signature !== 'string' || !platform.signature.trim()) {
      fail(`latest.json platform '${platformKey}' has no updater signature.`);
    }

    const { assetName } = parseBrowserDownloadUrl(platform.url, {
      repository,
      tag,
      label: `latest.json platform '${platformKey}' URL`,
    });
    if (!assets.has(assetName)) {
      fail(`latest.json platform '${platformKey}' points to '${assetName}', but that asset was not uploaded.`);
    }
  }
}

export function verifyReleaseDirectory({ inputDirectory, repository, tag, platformKeys = ALL_PLATFORM_KEYS }) {
  parseRepository(repository);
  assertSimpleTag(tag);
  const resolvedInputDirectory = resolve(inputDirectory);
  if (!existsSync(resolvedInputDirectory)) {
    fail(`Release asset directory does not exist: ${resolvedInputDirectory}`);
  }

  const assets = new Set(
    readdirSync(resolvedInputDirectory, { withFileTypes: true })
      .filter((entry) => entry.isFile())
      .map((entry) => entry.name),
  );
  const expectedVersion = tag.startsWith('v') ? tag.slice(1) : tag;
  const expectedInstallers = platformKeys.map((platformKey) => PLATFORM_INSTALLERS[platformKey](expectedVersion));
  for (const installer of expectedInstallers) {
    if (!assets.has(installer)) {
      fail(`Required installer asset is missing: ${installer}`);
    }
  }

  const latestJsonPath = resolve(resolvedInputDirectory, 'latest.json');
  if (!existsSync(latestJsonPath)) {
    fail('The release is missing latest.json. Signed in-app updates cannot work without it.');
  }
  let latest;
  try {
    latest = JSON.parse(readFileSync(latestJsonPath, 'utf8'));
  } catch (error) {
    fail(`Could not parse latest.json: ${error instanceof Error ? error.message : String(error)}`);
  }

  validateLatestJson({ latest, assets, repository, tag, platformKeys });
  console.log(`Release asset verification passed for ${tag}: installers and ${platformKeys.length} updater platforms are complete.`);
}

export function main(argumentsList = process.argv.slice(2)) {
  verifyReleaseDirectory(parseOptions(argumentsList));
}

const invokedPath = process.argv[1] ? pathToFileURL(resolve(process.argv[1])).href : undefined;
if (invokedPath === import.meta.url) {
  try {
    main();
  } catch (error) {
    console.error(`iHub release asset verification failed: ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 1;
  }
}
