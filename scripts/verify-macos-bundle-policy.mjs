import { readFile } from 'node:fs/promises';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const repositoryRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');
const tauriDirectory = resolve(repositoryRoot, 'src-tauri');
const configPath = resolve(tauriDirectory, 'tauri.conf.json');
const infoPlistPath = resolve(tauriDirectory, 'Info.plist');

const [configSource, infoPlist] = await Promise.all([
  readFile(configPath, 'utf8'),
  readFile(infoPlistPath, 'utf8'),
]);

let config;
try {
  config = JSON.parse(configSource);
} catch (error) {
  throw new Error(`Unable to parse ${configPath}: ${error.message}`);
}

if (config?.bundle?.macOS?.infoPlist !== './Info.plist') {
  throw new Error(
    'bundle.macOS.infoPlist must explicitly merge ./Info.plist into the packaged macOS app.',
  );
}

const uiElementEntries = [
  ...infoPlist.matchAll(
    /<key>\s*LSUIElement\s*<\/key>\s*<(true|false)\s*\/\s*>/giu,
  ),
];

if (uiElementEntries.length !== 1 || uiElementEntries[0][1].toLowerCase() !== 'true') {
  throw new Error(
    'src-tauri/Info.plist must define exactly one LSUIElement key with a true boolean value.',
  );
}

process.stdout.write(
  'macOS bundle policy verified: Info.plist is merged and LSUIElement is true.\n',
);
