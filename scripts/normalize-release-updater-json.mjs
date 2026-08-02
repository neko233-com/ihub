#!/usr/bin/env node
// Rewrites updater asset API URLs emitted by tauri-action to the canonical
// tagged download URLs that become live when the exact draft is published.
// GitHub exposes draft assets under a temporary `untagged-*` download segment,
// so every target URL is derived only after validating the draft release ID,
// tag, asset ID, name, state, API URL, and temporary browser URL.

import { readFileSync, writeFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

function fail(message) {
  throw new Error(message);
}

function strictPathSegment(value) {
  return encodeURIComponent(value).replace(/[!'()*]/g, (character) => `%${character.charCodeAt(0).toString(16).toUpperCase()}`);
}

export function parseRepository(repository) {
  if (typeof repository !== 'string' || !/^[A-Za-z0-9][A-Za-z0-9._-]*\/[A-Za-z0-9][A-Za-z0-9._-]*$/.test(repository)) {
    fail('repository must be in OWNER/REPO form.');
  }
  const [owner, repo] = repository.split('/');
  return { owner, repo };
}

export function assertSimpleTag(tag) {
  if (typeof tag !== 'string' || !/^v?[0-9A-Za-z][0-9A-Za-z.+-]*$/.test(tag)) {
    fail('tag must be a simple release tag such as v0.1.0.');
  }
  return tag;
}

function assertSafeAssetName(assetName, label) {
  if (
    typeof assetName !== 'string'
    || !assetName
    || assetName === '.'
    || assetName === '..'
    || assetName.includes('/')
    || assetName.includes('\\')
    || /[\u0000-\u001F\u007F]/u.test(assetName)
  ) {
    fail(`${label} has an unsafe asset name.`);
  }
  return assetName;
}

function parseStrictHttpsUrl(value, label) {
  if (typeof value !== 'string' || !value || value !== value.trim()) {
    fail(`${label} must be a non-empty URL without surrounding whitespace.`);
  }

  let parsed;
  try {
    parsed = new URL(value);
  } catch {
    fail(`${label} is not a valid URL.`);
  }
  if (parsed.protocol !== 'https:') {
    fail(`${label} must use HTTPS.`);
  }
  if (parsed.username || parsed.password) {
    fail(`${label} must not contain user information.`);
  }

  const authorityStart = 'https://'.length;
  const authorityEndRelative = value.slice(authorityStart).search(/[/?#]/u);
  const authorityEnd = authorityEndRelative === -1 ? value.length : authorityStart + authorityEndRelative;
  const rawAuthority = value.slice(authorityStart, authorityEnd);
  if (rawAuthority.includes('@')) {
    fail(`${label} must not contain user information.`);
  }
  // Fixed GitHub hosts never require an explicit port. Comparing the raw
  // authority also catches a default :443 port, which URL normalizes away.
  if (rawAuthority.toLowerCase() !== parsed.hostname.toLowerCase()) {
    fail(`${label} must not contain a port or encoded host.`);
  }
  if (parsed.port) {
    fail(`${label} must not contain a port.`);
  }
  if (parsed.search) {
    fail(`${label} must not contain a query string.`);
  }
  if (parsed.hash) {
    fail(`${label} must not contain a fragment.`);
  }

  return parsed;
}

function decodePathSegments(parsed, label) {
  const encodedSegments = parsed.pathname.split('/').slice(1);
  return encodedSegments.map((encodedSegment) => {
    let decodedSegment;
    try {
      decodedSegment = decodeURIComponent(encodedSegment);
    } catch {
      fail(`${label} has an invalid escaped path.`);
    }
    if (decodedSegment.includes('/') || decodedSegment.includes('\\') || decodedSegment.includes('\0')) {
      fail(`${label} has an encoded path separator.`);
    }
    return decodedSegment;
  });
}

export function browserDownloadUrl(repository, tag, assetName) {
  const { owner, repo } = parseRepository(repository);
  assertSimpleTag(tag);
  assertSafeAssetName(assetName, 'Release asset');
  return `https://github.com/${strictPathSegment(owner)}/${strictPathSegment(repo)}/releases/download/${strictPathSegment(tag)}/${strictPathSegment(assetName)}`;
}

function releaseApiUrl(repository, releaseId) {
  const { owner, repo } = parseRepository(repository);
  return `https://api.github.com/repos/${strictPathSegment(owner)}/${strictPathSegment(repo)}/releases/${releaseId}`;
}

function assetApiUrl(repository, assetId) {
  const { owner, repo } = parseRepository(repository);
  return `https://api.github.com/repos/${strictPathSegment(owner)}/${strictPathSegment(repo)}/releases/assets/${assetId}`;
}

function normalizePositiveInteger(value, label) {
  const stringValue = String(value);
  if (!/^[1-9][0-9]*$/u.test(stringValue)) {
    fail(`${label} must be a positive integer.`);
  }
  return stringValue;
}

function assertCanonicalUrl(value, expected, label) {
  parseStrictHttpsUrl(value, label);
  if (value !== expected) {
    fail(`${label} does not match the canonical URL for this release.`);
  }
}

function validateDraftAssetBrowserUrl(value, {
  repository,
  tag,
  assetName,
  label,
}) {
  const parsed = parseStrictHttpsUrl(value, label);
  if (parsed.hostname.toLowerCase() !== 'github.com') {
    fail(`${label} must use github.com.`);
  }

  const segments = decodePathSegments(parsed, label);
  const [owner, repo] = repository.split('/');
  if (
    segments.length !== 6
    || segments[0] !== owner
    || segments[1] !== repo
    || segments[2] !== 'releases'
    || segments[3] !== 'download'
    || segments[5] !== assetName
  ) {
    fail(`${label} is not the expected draft asset download URL.`);
  }

  const downloadSegment = segments[4];
  if (downloadSegment !== tag && !/^untagged-[0-9a-f]{12,64}$/u.test(downloadSegment)) {
    fail(`${label} has an invalid draft download segment.`);
  }
  const expected = `https://github.com/${strictPathSegment(owner)}/${strictPathSegment(repo)}/releases/download/${strictPathSegment(downloadSegment)}/${strictPathSegment(assetName)}`;
  if (value !== expected) {
    fail(`${label} must use the exact GitHub draft asset URL.`);
  }
  return downloadSegment;
}

export function parseBrowserDownloadUrl(value, { repository, tag, label = 'Updater URL' }) {
  parseRepository(repository);
  assertSimpleTag(tag);
  const parsed = parseStrictHttpsUrl(value, label);
  if (parsed.hostname.toLowerCase() !== 'github.com') {
    fail(`${label} must use github.com.`);
  }

  const segments = decodePathSegments(parsed, label);
  if (segments.length !== 6 || segments[2] !== 'releases' || segments[3] !== 'download') {
    fail(`${label} is not a GitHub release browser download URL.`);
  }
  const [owner, repo] = repository.split('/');
  if (segments[0] !== owner || segments[1] !== repo || segments[4] !== tag) {
    fail(`${label} must point to the current repository and release tag.`);
  }

  const assetName = assertSafeAssetName(segments[5], label);
  const expected = browserDownloadUrl(repository, tag, assetName);
  if (value !== expected) {
    fail(`${label} must use the canonical browser download URL.`);
  }
  return { assetName, url: expected };
}

function parseApiAssetUrl(value, { repository, label }) {
  parseRepository(repository);
  const parsed = parseStrictHttpsUrl(value, label);
  if (parsed.hostname.toLowerCase() !== 'api.github.com') {
    fail(`${label} must use api.github.com or github.com.`);
  }

  const segments = decodePathSegments(parsed, label);
  const [owner, repo] = repository.split('/');
  if (
    segments.length !== 6
    || segments[0] !== 'repos'
    || segments[1] !== owner
    || segments[2] !== repo
    || segments[3] !== 'releases'
    || segments[4] !== 'assets'
  ) {
    fail(`${label} must be an asset API URL for the current repository.`);
  }
  const assetId = normalizePositiveInteger(segments[5], `${label} asset ID`);
  const expected = assetApiUrl(repository, assetId);
  if (value !== expected) {
    fail(`${label} must use the canonical asset API URL.`);
  }
  return assetId;
}

export function indexDraftReleaseAssets({ releaseMetadata, repository, tag, releaseId }) {
  parseRepository(repository);
  assertSimpleTag(tag);
  const expectedReleaseId = normalizePositiveInteger(releaseId, 'release ID');
  if (!releaseMetadata || typeof releaseMetadata !== 'object' || Array.isArray(releaseMetadata)) {
    fail('Release metadata must contain an object.');
  }
  const metadataReleaseId = normalizePositiveInteger(releaseMetadata.id, 'Release metadata ID');
  if (metadataReleaseId !== expectedReleaseId) {
    fail('Release metadata ID does not match the requested draft release.');
  }
  if (releaseMetadata.tag_name !== tag) {
    fail('Release metadata tag does not match the requested release tag.');
  }
  if (releaseMetadata.draft !== true) {
    fail('Release metadata must describe a draft release.');
  }
  assertCanonicalUrl(
    releaseMetadata.url,
    releaseApiUrl(repository, expectedReleaseId),
    'Release metadata API URL',
  );
  if (!Array.isArray(releaseMetadata.assets)) {
    fail('Release metadata must contain an assets array.');
  }

  const byId = new Map();
  const byBrowserUrl = new Map();
  const names = new Set();
  const downloadSegments = new Set();
  for (const asset of releaseMetadata.assets) {
    if (!asset || typeof asset !== 'object' || Array.isArray(asset)) {
      fail('Release metadata contains an invalid asset.');
    }
    const assetId = normalizePositiveInteger(asset.id, 'Release asset ID');
    const name = assertSafeAssetName(asset.name, `Release asset ${assetId}`);
    if (asset.state !== 'uploaded') {
      fail(`Release asset '${name}' is not fully uploaded.`);
    }
    assertCanonicalUrl(asset.url, assetApiUrl(repository, assetId), `Release asset '${name}' API URL`);
    const expectedBrowserUrl = browserDownloadUrl(repository, tag, name);
    downloadSegments.add(validateDraftAssetBrowserUrl(asset.browser_download_url, {
      repository,
      tag,
      assetName: name,
      label: `Release asset '${name}' browser download URL`,
    }));
    if (byId.has(assetId)) {
      fail(`Release metadata repeats asset ID ${assetId}.`);
    }
    if (names.has(name)) {
      fail(`Release metadata repeats asset name '${name}'.`);
    }
    if (byBrowserUrl.has(expectedBrowserUrl)) {
      fail(`Release metadata repeats browser download URL '${expectedBrowserUrl}'.`);
    }

    const indexedAsset = { assetId, name, browserDownloadUrl: expectedBrowserUrl };
    byId.set(assetId, indexedAsset);
    byBrowserUrl.set(expectedBrowserUrl, indexedAsset);
    names.add(name);
  }
  if (downloadSegments.size > 1) {
    fail('Release metadata mixes multiple draft download segments.');
  }
  return { byId, byBrowserUrl };
}

export function normalizeUpdaterJson({ latest, releaseMetadata, repository, tag, releaseId }) {
  if (!latest || typeof latest !== 'object' || Array.isArray(latest)) {
    fail('latest.json must contain an object.');
  }
  if (!latest.platforms || typeof latest.platforms !== 'object' || Array.isArray(latest.platforms)) {
    fail('latest.json must contain a platforms object.');
  }

  const assets = indexDraftReleaseAssets({ releaseMetadata, repository, tag, releaseId });
  const normalizedPlatforms = {};
  for (const [platformKey, platform] of Object.entries(latest.platforms)) {
    if (!platform || typeof platform !== 'object' || Array.isArray(platform)) {
      fail(`latest.json platform '${platformKey}' must contain an object.`);
    }
    if (typeof platform.url !== 'string' || !platform.url) {
      fail(`latest.json platform '${platformKey}' has no update URL.`);
    }

    const label = `latest.json platform '${platformKey}' URL`;
    let indexedAsset;
    let parsed;
    try {
      parsed = parseStrictHttpsUrl(platform.url, label);
    } catch (error) {
      throw error;
    }
    if (parsed.hostname.toLowerCase() === 'api.github.com') {
      const assetId = parseApiAssetUrl(platform.url, { repository, label });
      indexedAsset = assets.byId.get(assetId);
      if (!indexedAsset) {
        fail(`${label} references asset ID ${assetId}, which is not in this draft release.`);
      }
    } else if (parsed.hostname.toLowerCase() === 'github.com') {
      const { url } = parseBrowserDownloadUrl(platform.url, { repository, tag, label });
      indexedAsset = assets.byBrowserUrl.get(url);
      if (!indexedAsset) {
        fail(`${label} is not present in this draft release's asset metadata.`);
      }
    } else {
      fail(`${label} must use api.github.com or github.com.`);
    }

    normalizedPlatforms[platformKey] = {
      ...platform,
      url: indexedAsset.browserDownloadUrl,
    };
  }

  return {
    ...latest,
    platforms: normalizedPlatforms,
  };
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

function parseOptions(argumentsList) {
  if (argumentsList.includes('--help') || argumentsList.includes('-h')) {
    console.log(`Usage: node scripts/normalize-release-updater-json.mjs \\
  --input latest.json --output normalized/latest.json \\
  --release-metadata release.json --release-id ID \\
  --repository OWNER/REPO --tag vX.Y.Z`);
    process.exit(0);
  }

  const knownOptions = new Set([
    '--input',
    '--output',
    '--release-metadata',
    '--release-id',
    '--repository',
    '--tag',
  ]);
  for (let index = 0; index < argumentsList.length; index += 1) {
    const argument = argumentsList[index];
    if (!knownOptions.has(argument)) {
      fail(`Unknown option: ${argument}`);
    }
    index += 1;
  }

  const options = {
    input: readOption(argumentsList, '--input'),
    output: readOption(argumentsList, '--output'),
    releaseMetadata: readOption(argumentsList, '--release-metadata'),
    releaseId: readOption(argumentsList, '--release-id'),
    repository: readOption(argumentsList, '--repository'),
    tag: readOption(argumentsList, '--tag'),
  };
  if (Object.values(options).some((value) => !value)) {
    fail('--input, --output, --release-metadata, --release-id, --repository, and --tag are required.');
  }
  parseRepository(options.repository);
  assertSimpleTag(options.tag);
  normalizePositiveInteger(options.releaseId, 'release ID');
  return options;
}

function readJson(path, label) {
  try {
    return JSON.parse(readFileSync(resolve(path), 'utf8'));
  } catch (error) {
    fail(`Could not parse ${label}: ${error instanceof Error ? error.message : String(error)}`);
  }
}

export function main(argumentsList = process.argv.slice(2)) {
  const options = parseOptions(argumentsList);
  const latest = readJson(options.input, 'latest.json');
  const releaseMetadata = readJson(options.releaseMetadata, 'release metadata');
  const normalized = normalizeUpdaterJson({
    latest,
    releaseMetadata,
    repository: options.repository,
    tag: options.tag,
    releaseId: options.releaseId,
  });
  writeFileSync(resolve(options.output), `${JSON.stringify(normalized, null, 2)}\n`, 'utf8');
  console.log(`Normalized ${Object.keys(normalized.platforms).length} updater URL(s) for release ${options.tag}.`);
}

const invokedPath = process.argv[1] ? pathToFileURL(resolve(process.argv[1])).href : undefined;
if (invokedPath === import.meta.url) {
  try {
    main();
  } catch (error) {
    console.error(`iHub updater manifest normalization failed: ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 1;
  }
}
