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
