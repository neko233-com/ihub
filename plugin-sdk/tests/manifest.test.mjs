import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import { fileURLToPath } from "node:url";

import { validateManifest } from "../dist/manifest.js";

const baseManifest = () => ({
  schemaVersion: 1,
  id: "ihub-plugin-test",
  name: "Test",
  version: "1.0.0",
  engines: { ihub: ">=0.1.0", api: "^1.0.0" },
  entry: { frontend: "dist/index.html" },
  permissions: {},
});

function withIcon(icon) {
  return { ...baseManifest(), icon };
}

test("accepts normal package-relative raster artwork paths", () => {
  for (const icon of [
    "public/icon.png",
    "assets/artwork/tool.jpeg",
    "assets\\artwork\\tool.webp",
    "assets/console-tool.png",
  ]) {
    assert.equal(validateManifest(withIcon(icon)).valid, true, icon);
  }
});

test("rejects dangerous or ambiguous artwork components", () => {
  const unsafe = [
    "",
    "/absolute.png",
    "\\\\server\\share.png",
    "../escape.png",
    "art/../escape.png",
    "art/./icon.png",
    "art//icon.png",
    "art\\icon.png\\",
    "art/icon.",
    "art/icon ",
    "C:\\icon.png",
    "art/name:stream.png",
    "art/\u0000icon.png",
    "art/\u001ficon.png",
    "art/\u007ficon.png",
    "art/\u0085icon.png",
    "CON",
    "public/con.png",
    "public/PrN.jpeg",
    "public/AUX.webp",
    "public/nul.anything.png",
    "public/COM1.png",
    "public/lPt9",
  ];

  for (const icon of unsafe) {
    const result = validateManifest(withIcon(icon));
    assert.equal(result.valid, false, JSON.stringify(icon));
    assert.ok(result.issues.some((issue) => issue.path === "$.icon"), JSON.stringify(icon));
  }
});

test("limits static commands and distinct artwork candidates", () => {
  const sixtyFour = Array.from({ length: 64 }, (_, index) => ({
    id: `c${index}`,
    title: `Command ${index}`,
    icon: "public/shared.png",
  }));
  assert.equal(
    validateManifest({ ...baseManifest(), contributes: { commands: sixtyFour } }).valid,
    true,
  );

  const tooManyCommands = [...sixtyFour, { id: "overflow", title: "Overflow" }];
  const commandResult = validateManifest({
    ...baseManifest(),
    contributes: { commands: tooManyCommands },
  });
  assert.ok(commandResult.issues.some((issue) => issue.path === "$.contributes.commands"));

  const distinctArtwork = Array.from({ length: 33 }, (_, index) => ({
    id: `a${index}`,
    title: `Artwork ${index}`,
    icon: `public/artwork-${index}.png`,
  }));
  const artworkResult = validateManifest({
    ...baseManifest(),
    contributes: { commands: distinctArtwork },
  });
  assert.ok(
    artworkResult.issues.some((issue) =>
      issue.message.includes("at most 32 distinct artwork paths"),
    ),
  );
});

test("schema mirrors command and artwork path limits", () => {
  const schemaPath = fileURLToPath(new URL("../manifest.schema.json", import.meta.url));
  const schema = JSON.parse(readFileSync(schemaPath, "utf8"));
  assert.equal(schema.properties.contributes.properties.commands.maxItems, 64);

  const artworkPattern = new RegExp(schema.$defs.artworkPath.allOf[1].pattern);
  assert.equal(artworkPattern.test("public/icon.png"), true);
  for (const unsafe of [
    "public/CON.png",
    "public/name:stream.png",
    "public/icon.",
    "public//icon.png",
    "public/./icon.png",
    "public/\u0000icon.png",
    "public/\u0085icon.png",
  ]) {
    assert.equal(artworkPattern.test(unsafe), false, JSON.stringify(unsafe));
  }
});

test("accepts only an explicit boolean microphone permission", () => {
  const declared = validateManifest({
    ...baseManifest(),
    permissions: { microphone: true },
  });
  assert.deepEqual(declared.issues, []);

  const nonBoolean = validateManifest({
    ...baseManifest(),
    permissions: { microphone: "yes" },
  });
  assert.ok(nonBoolean.issues.some(
    (issue) => issue.path === "$.permissions.microphone"
      && issue.message.includes("boolean"),
  ));

  const unknown = validateManifest({
    ...baseManifest(),
    permissions: { microhpone: true },
  });
  assert.ok(unknown.issues.some(
    (issue) => issue.path === "$.permissions.microhpone"
      && issue.message.includes("not supported"),
  ));
});

test("schema declares microphone as a strict boolean permission", () => {
  const schemaPath = fileURLToPath(new URL("../manifest.schema.json", import.meta.url));
  const schema = JSON.parse(readFileSync(schemaPath, "utf8"));
  const permissions = schema.properties.permissions;

  assert.equal(permissions.additionalProperties, false);
  assert.equal(permissions.properties.microphone.type, "boolean");
});

test("validates every nested permission object and bounded declaration list", () => {
  const valid = validateManifest({
    ...baseManifest(),
    permissions: {
      filesystem: { read: ["user-selected"], write: [] },
      network: { allow: ["https://api.example.test"] },
      clipboard: { read: true, write: false, history: true },
      process: { spawn: true, allow: ["trusted-worker"] },
      shell: { openExternal: true, openPath: false },
    },
  });
  assert.deepEqual(valid.issues, []);

  const invalidPermissions = [
    { network: "https://api.example.test" },
    { network: { typo: true } },
    { network: { allow: "https://api.example.test" } },
    { network: { allow: [""] } },
    { network: { allow: [" https://api.example.test"] } },
    { network: { allow: ["\uFEFFhttps://api.example.test"] } },
    { network: { allow: ["https://api.example.test\u0001"] } },
    { network: { allow: ["https://api.example.test", "https://api.example.test"] } },
    { network: { allow: Array.from({ length: 65 }, (_, index) => `target-${index}`) } },
    { network: { allow: ["x".repeat(513)] } },
    { filesystem: { read: true } },
    { filesystem: { typo: [] } },
    { clipboard: { history: "yes" } },
    { clipboard: { typo: true } },
    { process: { spawn: "yes" } },
    { process: { allow: [""] } },
    { shell: { openExternal: "yes" } },
    { shell: { typo: true } },
  ];
  for (const permissions of invalidPermissions) {
    const result = validateManifest({ ...baseManifest(), permissions });
    assert.equal(result.valid, false, JSON.stringify(permissions));
    assert.ok(
      result.issues.some((issue) => issue.path.startsWith("$.permissions.")),
      JSON.stringify(permissions),
    );
  }
});

test("schema mirrors bounded permission declaration lists", () => {
  const schemaPath = fileURLToPath(new URL("../manifest.schema.json", import.meta.url));
  const schema = JSON.parse(readFileSync(schemaPath, "utf8"));
  for (const definition of [
    schema.$defs.pathScopeList,
    schema.$defs.permissionStringList,
  ]) {
    assert.equal(definition.maxItems, 64);
    assert.equal(definition.uniqueItems, true);
    assert.equal(definition.items.maxLength, 512);
    const pattern = new RegExp(definition.items.pattern);
    assert.equal(pattern.test("user-configured HTTPS endpoint"), true);
    assert.equal(pattern.test(" leading-space"), false);
    assert.equal(pattern.test("trailing-space "), false);
    assert.equal(pattern.test("\uFEFFleading-bom"), false);
    assert.equal(pattern.test("control\u0001"), false);
  }
});

test("accepts only permissioned, unique shortcut-to-command or keyword mappings", () => {
  const valid = validateManifest({
    ...baseManifest(),
    permissions: { globalShortcut: true },
    contributes: {
      commands: [{
        id: "open",
        title: "Open",
        keywords: ["launch", "打开"],
        shortcut: "alt + keyo",
      }],
      globalShortcuts: [{
        id: "find",
        shortcut: "CmdOrCtrl+Alt+KeyF",
        keyword: "find files",
      }, {
        id: "run-open",
        shortcut: "Alt+Shift+KeyO",
        commandId: "open",
      }],
    },
  });
  assert.deepEqual(valid.issues, []);

  const missingPermission = validateManifest({
    ...baseManifest(),
    contributes: {
      commands: [{ id: "open", title: "Open", shortcut: "Alt+KeyO" }],
    },
  });
  assert.ok(missingPermission.issues.some((issue) => issue.message.includes("globalShortcut")));

  const invalidTargets = validateManifest({
    ...baseManifest(),
    permissions: { globalShortcut: true },
    contributes: {
      commands: [{ id: "open", title: "Open" }],
      globalShortcuts: [{
        id: "unsafe",
        shortcut: "Alt+F4",
        commandId: "missing",
        keyword: "both",
      }],
    },
  });
  assert.ok(invalidTargets.issues.some((issue) => issue.path.endsWith(".shortcut")));
  assert.ok(invalidTargets.issues.some((issue) => issue.message.includes("exactly one")));
});

test("blocks launcher-owned and duplicate plugin accelerators", () => {
  const result = validateManifest({
    ...baseManifest(),
    permissions: { globalShortcut: true },
    contributes: {
      commands: [
        { id: "open", title: "Open", shortcut: "Alt+Space" },
        { id: "other", title: "Other", shortcut: "Alt+KeyO" },
      ],
      globalShortcuts: [{
        id: "duplicate",
        shortcut: "alt+keyo",
        keyword: "other",
      }],
    },
  });
  assert.ok(result.issues.some((issue) => issue.message.includes("reserved")));
  assert.ok(result.issues.some((issue) => issue.message.includes("duplicates")));
});
