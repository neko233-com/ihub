#!/usr/bin/env node
/**
 * Refuse a GitHub Release that cannot be offered as a forward Tauri updater.
 *
 * This intentionally compares every published SemVer release, rather than
 * trusting creation date or the GitHub "latest" endpoint. A prerelease can be
 * newer than a stable release by date while still sorting below it in SemVer.
 */

import { fileURLToPath } from 'node:url';
import { resolve } from 'node:path';

const REPOSITORY_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._-]*\/[A-Za-z0-9][A-Za-z0-9._-]*$/;
const MAX_RELEASE_PAGES = 100;

function usage() {
  return [
    'Usage: node scripts/verify-release-version.mjs --repository OWNER/REPO --tag vX.Y.Z',
    '',
    'Requires GITHUB_TOKEN and queries the configured GITHUB_API_URL (or api.github.com).',
  ].join('\n');
}

function parseArguments(argv) {
  const options = {
    repository: process.env.GITHUB_REPOSITORY ?? '',
    tag: process.env.IHUB_RELEASE_TAG ?? '',
  };

  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    if (argument === '--repository' || argument === '--tag') {
      const value = argv[index + 1];
      if (!value || value.startsWith('--')) {
        throw new Error(`${argument} requires a value.`);
      }
      options[argument.slice(2)] = value;
      index += 1;
      continue;
    }
    if (argument === '--help' || argument === '-h') {
      return { help: true, ...options };
    }
    throw new Error(`Unknown option: ${argument}`);
  }

  return options;
}

export function parseSemver(tag) {
  const value = String(tag).replace(/^v/i, '');
  const match = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)(?:-([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?(?:\+([0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*))?$/.exec(value);
  if (!match) return null;

  const prerelease = match[4] ? match[4].split('.') : [];
  if (prerelease.some((identifier) => /^\d+$/.test(identifier) && !/^(0|[1-9]\d*)$/.test(identifier))) {
    return null;
  }

  return {
    tag: String(tag),
    core: [match[1], match[2], match[3]].map((part) => BigInt(part)),
    prerelease,
  };
}

export function compareSemver(left, right) {
  for (let index = 0; index < left.core.length; index += 1) {
    if (left.core[index] !== right.core[index]) return left.core[index] > right.core[index] ? 1 : -1;
  }

  if (left.prerelease.length === 0 || right.prerelease.length === 0) {
    if (left.prerelease.length === right.prerelease.length) return 0;
    return left.prerelease.length === 0 ? 1 : -1;
  }

  const length = Math.max(left.prerelease.length, right.prerelease.length);
  for (let index = 0; index < length; index += 1) {
    const a = left.prerelease[index];
    const b = right.prerelease[index];
    if (a === undefined || b === undefined) return a === undefined ? -1 : 1;
    if (a === b) continue;

    const aNumeric = /^\d+$/.test(a);
    const bNumeric = /^\d+$/.test(b);
    if (aNumeric && bNumeric) return BigInt(a) > BigInt(b) ? 1 : -1;
    if (aNumeric !== bNumeric) return aNumeric ? -1 : 1;
    return a > b ? 1 : -1;
  }
  return 0;
}

async function fetchPublishedReleases({ repository, token, apiRoot }) {
  const baseUrl = new URL(apiRoot.endsWith('/') ? apiRoot : `${apiRoot}/`);
  if (baseUrl.protocol !== 'https:') {
    throw new Error(`Refusing to send the GitHub token to a non-HTTPS API endpoint: ${baseUrl.toString()}`);
  }

  const [owner, repo] = repository.split('/');
  const releases = [];
  for (let page = 1; page <= MAX_RELEASE_PAGES; page += 1) {
    const url = new URL(`repos/${encodeURIComponent(owner)}/${encodeURIComponent(repo)}/releases`, baseUrl);
    url.searchParams.set('per_page', '100');
    url.searchParams.set('page', String(page));
    const response = await fetch(url, {
      headers: {
        Accept: 'application/vnd.github+json',
        Authorization: `Bearer ${token}`,
        'X-GitHub-Api-Version': '2022-11-28',
        'User-Agent': 'iHub-release-preflight',
      },
    });
    if (!response.ok) {
      throw new Error(`Could not list prior releases (${response.status} ${response.statusText}).`);
    }

    const pageReleases = await response.json();
    if (!Array.isArray(pageReleases)) {
      throw new Error('GitHub releases API returned an unexpected payload.');
    }
    releases.push(...pageReleases);
    if (pageReleases.length < 100) return releases;
  }

  throw new Error(`Refusing to compare more than ${MAX_RELEASE_PAGES * 100} releases; narrow the release history or raise the explicit limit.`);
}

export async function verifyReleaseVersion({ repository, tag, token, apiRoot }) {
  if (!REPOSITORY_PATTERN.test(repository)) {
    throw new Error('Repository must be in owner/repository form.');
  }
  if (!token) {
    throw new Error('GITHUB_TOKEN is required for release version preflight.');
  }

  const target = parseSemver(tag);
  if (!target) {
    throw new Error(`Requested release tag '${tag}' is not SemVer.`);
  }

  let latest = null;
  for (const release of await fetchPublishedReleases({ repository, token, apiRoot })) {
    if (release?.draft === true) continue;
    const version = parseSemver(release?.tag_name);
    if (!version) {
      throw new Error(`Published release '${String(release?.tag_name)}' is not SemVer; refusing an unsafe version comparison.`);
    }
    if (!latest || compareSemver(version, latest) > 0) latest = version;
  }

  if (!latest) {
    return `Release version preflight passed: ${tag} is the first published SemVer release.`;
  }
  if (compareSemver(target, latest) <= 0) {
    throw new Error(`Requested release ${tag} must be greater than the latest published release ${latest.tag}; Tauri will not offer same-version or downgrade updates.`);
  }
  return `Release version preflight passed: ${tag} > ${latest.tag}.`;
}

async function main() {
  const options = parseArguments(process.argv.slice(2));
  if (options.help) {
    process.stdout.write(`${usage()}\n`);
    return;
  }
  const message = await verifyReleaseVersion({
    repository: options.repository,
    tag: options.tag,
    token: process.env.GITHUB_TOKEN ?? '',
    apiRoot: process.env.GITHUB_API_URL ?? 'https://api.github.com',
  });
  process.stdout.write(`${message}\n`);
}

const invokedPath = process.argv[1] ? resolve(process.argv[1]) : '';
if (invokedPath === fileURLToPath(import.meta.url)) {
  main().catch((error) => {
    process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`);
    process.exitCode = 1;
  });
}
