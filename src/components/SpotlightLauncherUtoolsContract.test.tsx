import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { SpotlightLauncher } from "./SpotlightLauncher";

function launcherStyles() {
  const markup = renderToStaticMarkup(
    <SpotlightLauncher
      onClose={() => undefined}
      open
    />,
  );
  const match = markup.match(/<style>([\s\S]*?)<\/style>/);
  expect(match).not.toBeNull();
  const styles = match?.[1] ?? "";
  const contractIndex = styles.indexOf("/* Apple high-saturation launcher contract.");
  expect(contractIndex).toBeGreaterThan(-1);
  return styles.slice(contractIndex);
}

function selectorRules(styles: string, selector: string) {
  const escapedSelector = selector.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
  return Array.from(
    styles.matchAll(new RegExp(`${escapedSelector}\\s*\\{([^}]*)\\}`, "g")),
    (match) => match[1] ?? "",
  );
}

function expectOneRule(
  styles: string,
  selector: string,
  declarations: readonly RegExp[],
) {
  const rules = selectorRules(styles, selector);
  expect(
    rules.some((rule) => declarations.every((declaration) => declaration.test(rule))),
    `${selector} should contain ${declarations.map(String).join(", ")}`,
  ).toBe(true);
}

describe("SpotlightLauncher Apple high-saturation visual contract", () => {
  it("lets the main WebView inherit the operating-system color scheme", () => {
    const tauriConfig = JSON.parse(
      readFileSync(new URL("../../src-tauri/tauri.conf.json", import.meta.url), "utf8"),
    ) as { app: { windows: Array<{ label: string; theme?: string }> } };
    const mainWindow = tauriConfig.app.windows.find(({ label }) => label === "main");

    expect(mainWindow).toBeDefined();
    expect(mainWindow).not.toHaveProperty("theme");
  });

  it("declares the measured Apple material and high-saturation tokens", () => {
    const styles = launcherStyles();

    expect(styles).toMatch(/--ihub-apple-surface:\s*#f5f5f7/i);
    expect(styles).toMatch(/--ihub-apple-material:\s*rgba\(255,\s*255,\s*255,\s*\.76\)/i);
    expect(styles).toMatch(/--ihub-apple-input:\s*#0a84ff/i);
    expect(styles).toMatch(/--ihub-apple-hover:\s*rgba\(94,\s*92,\s*230,\s*\.10\)/i);
    expect(styles).toMatch(/--ihub-apple-selected:\s*rgba\(10,\s*132,\s*255,\s*\.18\)/i);
    expect(styles).toMatch(/color-scheme:\s*light/i);
    expect(styles).not.toMatch(/--ihub-apple-surface:\s*#f4f4f4/i);
    expect(styles).not.toMatch(/@media \(prefers-color-scheme: light\)/i);
  });

  it("keeps hover feedback distinct from keyboard selection", () => {
    const styles = launcherStyles();

    expect(styles).not.toMatch(
      /\.ihub-spotlight__tile:hover[^{}]*\.ihub-spotlight__tile\.is-keyboard-selected\s*\{/s,
    );
    expect(styles).not.toMatch(
      /\.ihub-spotlight__result-row:hover[^{}]*\.ihub-spotlight__result-row\.is-keyboard-selected\s*\{/s,
    );
    expectOneRule(styles, ".ihub-spotlight__tile:hover:not(.is-keyboard-selected)", [
      /background(?:-color)?:\s*var\(--ihub-apple-hover\)/,
    ]);
    expectOneRule(styles, ".ihub-spotlight__tile.is-keyboard-selected", [
      /background(?:-color)?:\s*var\(--ihub-apple-selected\)/,
    ]);
    expectOneRule(styles, ".ihub-spotlight__result-row:hover:not(.is-keyboard-selected)", [
      /background(?:-color)?:\s*var\(--ihub-apple-hover\)/,
    ]);
    expectOneRule(styles, ".ihub-spotlight__result-row.is-keyboard-selected", [
      /background(?:-color)?:\s*var\(--ihub-apple-selected\)/,
    ]);
  });

  it("uses the installed launcher's measured search, tile, icon, and result sizes", () => {
    const styles = launcherStyles();

    expectOneRule(styles, ".ihub-spotlight__search-row", [
      /height:\s*56px/,
    ]);
    expectOneRule(styles, ".ihub-spotlight__tile", [
      /height:\s*86px/,
      /width:\s*86px/,
    ]);
    expectOneRule(styles, ".ihub-spotlight__tile-icon", [
      /height:\s*32px/,
      /width:\s*32px/,
    ]);
    expectOneRule(styles, ".ihub-spotlight__result-row", [
      /height:\s*48px/,
    ]);
    expectOneRule(styles, ".ihub-spotlight__result-icon", [
      /height:\s*32px/,
      /width:\s*32px/,
    ]);
  });

  it("keeps narrow-window grid geometry aligned with keyboard navigation", () => {
    const styles = launcherStyles();
    const narrowMediaIndex = styles.indexOf("@media (max-width: 355px)");
    const source = readFileSync(new URL("./SpotlightLauncher.tsx", import.meta.url), "utf8");

    expect(narrowMediaIndex).toBeGreaterThan(-1);
    expect(styles.slice(narrowMediaIndex)).toMatch(
      /\.ihub-spotlight__grid\s*\{[^}]*grid-template-columns:\s*repeat\(3,\s*86px\)/s,
    );
    expect(source).toMatch(
      /if \(window\.innerWidth <= 355\) \{\s*return 3;\s*\}/s,
    );
  });

  it("preserves transparent native-icon slots and contain sizing", () => {
    const styles = launcherStyles();

    expectOneRule(styles, ".ihub-spotlight__tile-icon.is-native", [
      /background:\s*transparent/,
    ]);
    expectOneRule(styles, ".ihub-spotlight__tile-icon.is-native img", [
      /object-fit:\s*contain/,
    ]);
    expectOneRule(styles, ".ihub-spotlight__result-icon.is-native", [
      /background:\s*transparent/,
    ]);
    expectOneRule(styles, ".ihub-spotlight__result-icon.is-native img", [
      /object-fit:\s*contain/,
    ]);
  });
});
