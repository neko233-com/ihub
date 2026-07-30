import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const stylesheet = readFileSync(new URL("../index.css", import.meta.url), "utf8");

describe("secondary surfaces uTools light appearance", () => {
  it("declares one semantic uTools palette for every managed surface", () => {
    expect(stylesheet).toMatch(
      /:root \{[^}]*--utools-surface: #f4f4f4;[^}]*--utools-card: #ffffff;[^}]*--utools-active: #d7d7d7;[^}]*--utools-border: #d0d0d0;[^}]*--utools-border-subtle: #e6e6e6;[^}]*--utools-text: #212121;[^}]*--utools-text-muted: #737373;[^}]*--utools-primary: #3f51b5;/s,
    );
    expect(stylesheet).toMatch(
      /--ihub-surface: var\(--utools-surface\);[^}]*--ihub-surface-raised: var\(--utools-card\);[^}]*--ihub-border: var\(--utools-border-subtle\);[^}]*--ihub-border-strong: var\(--utools-border\);[^}]*--ihub-text: var\(--utools-text\);[^}]*--ihub-accent: var\(--utools-primary\);/s,
    );
    expect(stylesheet).toMatch(
      /--utools-hover: rgba\(0, 0, 0, 0\.04\);[^}]*--utools-hover-strong: rgba\(0, 0, 0, 0\.05\);/s,
    );
    expect(stylesheet).toMatch(
      /--utools-danger: #a84249;[^}]*--utools-danger-hover: #872d2d;[^}]*--utools-danger-soft: #fbeaec;[^}]*--utools-danger-hover-soft: #f6dadd;[^}]*--utools-danger-border: #e2b6ba;[^}]*--utools-error: var\(--utools-danger\);/s,
    );
    expect(stylesheet).not.toMatch(/#3277a8|#327b70|rgba\(50,\s*119,\s*168/);
  });

  it("keeps settings on the shared management-center palette", () => {
    expect(stylesheet).toMatch(
      /\.app-shell--spotlight \.settings-panel \{[^}]*background: var\(--utools-surface\);[^}]*border-color: var\(--utools-border\);[^}]*color: var\(--utools-text\);[^}]*color-scheme: light;/s,
    );
    expect(stylesheet).toMatch(
      /\.app-shell--spotlight \.settings-switch \{[^}]*background: var\(--utools-active\);/s,
    );
    expect(stylesheet).toMatch(
      /\.app-shell--spotlight \.settings-switch\.is-on \{[^}]*background: var\(--utools-primary\);[^}]*border-color: var\(--utools-primary\);/s,
    );
    expect(stylesheet).toMatch(
      /\.app-shell--spotlight \.settings-action\.is-danger \{[^}]*background: var\(--utools-danger-soft\);[^}]*border-color: var\(--utools-danger-border\);[^}]*color: var\(--utools-danger\);/s,
    );
    expect(stylesheet).toMatch(
      /\.app-shell--spotlight \.settings-action\.is-danger:hover:not\(:disabled\) \{[^}]*background: var\(--utools-danger-hover-soft\);[^}]*border-color: var\(--utools-danger\);[^}]*color: var\(--utools-danger-hover\);/s,
    );
    expect(stylesheet).toMatch(
      /\.settings-hotkey-recorder:focus-visible,[^{]*\{[^}]*outline: 2px solid var\(--utools-primary\);/s,
    );
    expect(stylesheet).toMatch(
      /\.settings-error \{[^}]*color: var\(--utools-error\) !important;/s,
    );
  });

  it("keeps the built-in workbench on the shared light product palette", () => {
    expect(stylesheet).toMatch(
      /\.toolbox-drawer \{[^}]*background: var\(--utools-surface\);[^}]*border-color: var\(--utools-border\);[^}]*color: var\(--utools-text\);[^}]*color-scheme: light;/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-tab\.is-active \{[^}]*background: var\(--utools-active\);[^}]*border-color: var\(--utools-active\);[^}]*color: var\(--utools-text\);/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-code-input,\s*\.toolbox-calculator-input,\s*\.toolbox-field input \{[^}]*background: var\(--utools-card\);[^}]*border-color: var\(--utools-border\);[^}]*color: var\(--utools-text\);/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-primary-action,\s*\.toolbox-record-action,\s*\.toolbox-drawer \.accent-button \{[^}]*background: var\(--utools-primary\);[^}]*border: 1px solid var\(--utools-primary\);/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-danger-action \{[^}]*background: var\(--utools-danger-soft\);[^}]*border-color: var\(--utools-danger-border\);[^}]*color: var\(--utools-danger\);/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-danger-action:hover:not\(:disabled\) \{[^}]*background: var\(--utools-danger-hover-soft\);[^}]*border-color: var\(--utools-danger\);[^}]*color: var\(--utools-danger-hover\);/s,
    );
  });

  it("keeps embedded and detached plugin chrome on the same light palette", () => {
    expect(stylesheet).toMatch(
      /\.plugin-frame-overlay \{[^}]*background: var\(--utools-surface\);[^}]*border-color: var\(--utools-border\);[^}]*color: var\(--utools-text\);[^}]*color-scheme: light;/s,
    );
    expect(stylesheet).toMatch(
      /\.plugin-frame__header \{[^}]*background: var\(--utools-surface\);[^}]*border-color: var\(--utools-border\);/s,
    );
    expect(stylesheet).toMatch(
      /\.plugin-frame__tag,\s*\.plugin-frame__detach \{[^}]*background: var\(--utools-card\);[^}]*border-color: var\(--utools-border\);[^}]*color: var\(--utools-text\);/s,
    );
    expect(stylesheet).toMatch(
      /\.plugin-frame__detached-preview \{[^}]*background: var\(--utools-surface\);/s,
    );
  });

  it("preserves readable semantic state colors on light backgrounds", () => {
    expect(stylesheet).toMatch(
      /\.toolbox-feedback\.is-success \{[^}]*color: var\(--utools-success\);/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-feedback\.is-warning \{[^}]*color: var\(--utools-warning\);/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-feedback\.is-error \{[^}]*color: var\(--utools-error\);/s,
    );
  });

  it("keeps the bounded diagnostics viewer scrollable and readable", () => {
    expect(stylesheet).toMatch(
      /\.settings-log__viewport \{[^}]*background: var\(--ihub-surface-deep\);[^}]*border: 1px solid var\(--ihub-border\);[^}]*max-height: 220px;[^}]*overflow: auto;/s,
    );
    expect(stylesheet).toMatch(
      /\.settings-log__entry \{[^}]*display: grid;[^}]*font-family: "SFMono-Regular"[^}]*grid-template-columns: 54px 36px minmax\(70px, 105px\) minmax\(0, 1fr\);/s,
    );
    expect(stylesheet).toMatch(
      /\.settings-log__entry\.is-error \.settings-log__level \{[^}]*color: var\(--utools-error\);/s,
    );
  });
});
