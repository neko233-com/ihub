import assert from 'node:assert/strict';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import test from 'node:test';
import {
  browserDownloadUrl,
  normalizeUpdaterJson,
  parseBrowserDownloadUrl,
} from './normalize-release-updater-json.mjs';
import { validateLatestJson } from './verify-release-assets.mjs';

const REPOSITORY = 'neko233-com/ihub';
const TAG = 'v0.1.0';
const RELEASE_ID = 7001;
const ASSETS = [
  { id: 8101, name: 'ihub_0.1.0_windows_x64_setup.exe' },
  { id: 8102, name: 'ihub_0.1.0_darwin_aarch64.dmg' },
  { id: 8103, name: 'ihub_0.1.0_darwin_x64.dmg' },
];
const PLATFORM_ASSET = {
  'windows-x86_64': ASSETS[0],
  'darwin-aarch64': ASSETS[1],
  'darwin-x86_64': ASSETS[2],
};

function apiAssetUrl(assetId, repository = REPOSITORY) {
  return `https://api.github.com/repos/${repository}/releases/assets/${assetId}`;
}

function makeReleaseMetadata() {
  return {
    id: RELEASE_ID,
    tag_name: TAG,
    draft: true,
    url: `https://api.github.com/repos/${REPOSITORY}/releases/${RELEASE_ID}`,
    assets: ASSETS.map(({ id, name }) => ({
      id,
      name,
      state: 'uploaded',
      url: apiAssetUrl(id),
      browser_download_url: browserDownloadUrl(REPOSITORY, TAG, name),
    })),
  };
}

function makeLatest(urlFactory = ({ id }) => apiAssetUrl(id)) {
  return {
    version: '0.1.0',
    notes: 'Release notes',
    pub_date: '2026-07-29T00:00:00Z',
    platforms: Object.fromEntries(
      Object.entries(PLATFORM_ASSET).map(([platform, asset]) => [
        platform,
        {
          signature: `signature-${platform}`,
          url: urlFactory(asset, platform),
        },
      ]),
    ),
  };
}

test('normalizer maps only current draft asset API IDs to browser download URLs', () => {
  const latest = makeLatest();
  const normalized = normalizeUpdaterJson({
    latest,
    releaseMetadata: makeReleaseMetadata(),
    repository: REPOSITORY,
    tag: TAG,
    releaseId: RELEASE_ID,
  });

  assert.notStrictEqual(normalized, latest);
  assert.equal(normalized.notes, latest.notes);
  for (const [platform, asset] of Object.entries(PLATFORM_ASSET)) {
    assert.equal(
      normalized.platforms[platform].url,
      browserDownloadUrl(REPOSITORY, TAG, asset.name),
    );
    assert.equal(normalized.platforms[platform].signature, `signature-${platform}`);
  }
});

test('normalizer accepts canonical browser URLs only when metadata contains them', () => {
  const latest = makeLatest(({ name }) => browserDownloadUrl(REPOSITORY, TAG, name));
  const normalized = normalizeUpdaterJson({
    latest,
    releaseMetadata: makeReleaseMetadata(),
    repository: REPOSITORY,
    tag: TAG,
    releaseId: String(RELEASE_ID),
  });
  assert.deepEqual(normalized, latest);
});

test('normalizer rejects release metadata that is not the exact current draft', () => {
  const cases = [
    ['release ID', (metadata) => { metadata.id += 1; }],
    ['tag', (metadata) => { metadata.tag_name = 'v0.1.1'; }],
    ['draft state', (metadata) => { metadata.draft = false; }],
    ['release repository', (metadata) => {
      metadata.url = metadata.url.replace('/neko233-com/ihub/', '/other/ihub/');
    }],
    ['asset state', (metadata) => { metadata.assets[0].state = 'new'; }],
    ['asset API URL', (metadata) => { metadata.assets[0].url += '?token=unsafe'; }],
    ['browser URL', (metadata) => { metadata.assets[0].browser_download_url += '#fragment'; }],
    ['duplicate asset ID', (metadata) => { metadata.assets[1].id = metadata.assets[0].id; }],
    ['duplicate asset name', (metadata) => { metadata.assets[1].name = metadata.assets[0].name; }],
  ];

  for (const [name, mutate] of cases) {
    const metadata = makeReleaseMetadata();
    mutate(metadata);
    assert.throws(
      () => normalizeUpdaterJson({
        latest: makeLatest(),
        releaseMetadata: metadata,
        repository: REPOSITORY,
        tag: TAG,
        releaseId: RELEASE_ID,
      }),
      undefined,
      name,
    );
  }
});

test('normalizer rejects unknown asset IDs and updater URLs outside the current release', () => {
  const cases = [
    'https://api.github.com/repos/neko233-com/ihub/releases/assets/999999',
    'https://api.github.com/repos/other/ihub/releases/assets/8101',
    'https://github.com/neko233-com/ihub/releases/download/v0.1.0/not-uploaded.exe',
    'https://github.com/neko233-com/ihub/releases/download/v0.1.1/ihub_0.1.0_windows_x64_setup.exe',
    'https://downloads.example.com/ihub.exe',
  ];
  for (const url of cases) {
    const latest = makeLatest();
    latest.platforms['windows-x86_64'].url = url;
    assert.throws(() => normalizeUpdaterJson({
      latest,
      releaseMetadata: makeReleaseMetadata(),
      repository: REPOSITORY,
      tag: TAG,
      releaseId: RELEASE_ID,
    }), undefined, url);
  }
});

test('normalizer rejects userinfo, ports, query strings, fragments, and encoded path ambiguity', () => {
  const cases = [
    'https://attacker.invalid@api.github.com/repos/neko233-com/ihub/releases/assets/8101',
    'https://api.github.com:443/repos/neko233-com/ihub/releases/assets/8101',
    'https://api.github.com:444/repos/neko233-com/ihub/releases/assets/8101',
    'https://api.github.com/repos/neko233-com/ihub/releases/assets/8101?download=1',
    'https://api.github.com/repos/neko233-com/ihub/releases/assets/8101#fragment',
    'https://api.github.com/repos/%6Eeko233-com/ihub/releases/assets/8101',
    'https://api.github.com/repos/neko233-com/ihub/releases/assets/%38%31%30%31',
    'https://api.github.com/repos/neko233-com/ihub/releases/assets/8101/extra',
    'https://api.github.com/repos/neko233-com/ihub/releases/assets/%2F8101',
  ];
  for (const url of cases) {
    const latest = makeLatest();
    latest.platforms['windows-x86_64'].url = url;
    assert.throws(() => normalizeUpdaterJson({
      latest,
      releaseMetadata: makeReleaseMetadata(),
      repository: REPOSITORY,
      tag: TAG,
      releaseId: RELEASE_ID,
    }), undefined, url);
  }
});

test('browser URL parser accepts only a canonical current repo and tag URL', () => {
  const assetName = ASSETS[0].name;
  const expected = browserDownloadUrl(REPOSITORY, TAG, assetName);
  assert.deepEqual(
    parseBrowserDownloadUrl(expected, { repository: REPOSITORY, tag: TAG }),
    { assetName, url: expected },
  );

  const unsafeUrls = [
    apiAssetUrl(ASSETS[0].id),
    `https://attacker.invalid@github.com/${REPOSITORY}/releases/download/${TAG}/${assetName}`,
    `https://github.com:443/${REPOSITORY}/releases/download/${TAG}/${assetName}`,
    `https://github.com:444/${REPOSITORY}/releases/download/${TAG}/${assetName}`,
    `${expected}?download=1`,
    `${expected}#fragment`,
    expected.replace('/neko233-com/', '/%6Eeko233-com/'),
    expected.replace(`/${TAG}/`, '/%760.1.0/'),
    expected.replace(assetName, '%2E%2E%2Fevil.exe'),
    expected.replace(`/${assetName}`, `//${assetName}`),
    expected.replace(`/${assetName}`, `/${assetName}/extra`),
  ];
  for (const url of unsafeUrls) {
    assert.throws(
      () => parseBrowserDownloadUrl(url, { repository: REPOSITORY, tag: TAG }),
      undefined,
      url,
    );
  }
});

test('release verifier accepts canonical browser URLs and rejects API or cross-release URLs', () => {
  const assets = new Set(ASSETS.map(({ name }) => name));
  const canonicalLatest = makeLatest(({ name }) => browserDownloadUrl(REPOSITORY, TAG, name));
  assert.doesNotThrow(() => validateLatestJson({
    latest: canonicalLatest,
    assets,
    repository: REPOSITORY,
    tag: TAG,
  }));

  const unsafeUrls = [
    apiAssetUrl(ASSETS[0].id),
    browserDownloadUrl('other/ihub', TAG, ASSETS[0].name),
    browserDownloadUrl(REPOSITORY, 'v0.1.1', ASSETS[0].name),
    `${browserDownloadUrl(REPOSITORY, TAG, ASSETS[0].name)}?raw=1`,
  ];
  for (const url of unsafeUrls) {
    const latest = structuredClone(canonicalLatest);
    latest.platforms['windows-x86_64'].url = url;
    assert.throws(() => validateLatestJson({
      latest,
      assets,
      repository: REPOSITORY,
      tag: TAG,
    }), undefined, url);
  }
});

test('normalizer CLI writes a normalized manifest without changing signatures', () => {
  const fixtureDirectory = mkdtempSync(join(tmpdir(), 'ihub-release-json-test-'));
  try {
    const inputPath = join(fixtureDirectory, 'latest.json');
    const metadataPath = join(fixtureDirectory, 'release.json');
    const outputPath = join(fixtureDirectory, 'normalized.json');
    writeFileSync(inputPath, JSON.stringify(makeLatest()), 'utf8');
    writeFileSync(metadataPath, JSON.stringify(makeReleaseMetadata()), 'utf8');

    const result = spawnSync(
      process.execPath,
      [
        resolve('scripts/normalize-release-updater-json.mjs'),
        '--input',
        inputPath,
        '--output',
        outputPath,
        '--release-metadata',
        metadataPath,
        '--release-id',
        String(RELEASE_ID),
        '--repository',
        REPOSITORY,
        '--tag',
        TAG,
      ],
      { encoding: 'utf8' },
    );
    assert.equal(result.status, 0, result.stderr);
    const normalized = JSON.parse(readFileSync(outputPath, 'utf8'));
    assert.equal(
      normalized.platforms['windows-x86_64'].url,
      browserDownloadUrl(REPOSITORY, TAG, ASSETS[0].name),
    );
    assert.equal(
      normalized.platforms['windows-x86_64'].signature,
      'signature-windows-x86_64',
    );
  } finally {
    rmSync(fixtureDirectory, { recursive: true, force: true });
  }
});
