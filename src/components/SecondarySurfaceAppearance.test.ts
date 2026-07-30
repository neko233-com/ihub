import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const stylesheet = readFileSync(new URL("../index.css", import.meta.url), "utf8");

describe("secondary surfaces Apple high-saturation appearance", () => {
  it("declares one Apple semantic palette for every managed surface", () => {
    expect(stylesheet).toMatch(
      /:root \{[^}]*--apple-surface: #f5f5f7;[^}]*--apple-card: rgba\(255, 255, 255, 0\.84\);[^}]*--apple-active: rgba\(10, 132, 255, 0\.18\);[^}]*--apple-primary: #0a84ff;[^}]*--apple-indigo: #5e5ce6;[^}]*--apple-purple: #bf5af2;[^}]*--apple-pink: #ff375f;[^}]*--apple-orange: #ff9f0a;[^}]*--apple-cyan: #64d2ff;/s,
    );
    expect(stylesheet).toMatch(
      /--ihub-surface: var\(--apple-surface\);[^}]*--ihub-surface-raised: var\(--apple-card\);[^}]*--ihub-border: var\(--apple-border-subtle\);[^}]*--ihub-border-strong: var\(--apple-border\);[^}]*--ihub-text: var\(--apple-text\);[^}]*--ihub-accent: var\(--apple-primary\);/s,
    );
    expect(stylesheet).toMatch(
      /--apple-hover: rgba\(94, 92, 230, 0\.10\);[^}]*--apple-hover-strong: rgba\(94, 92, 230, 0\.16\);/s,
    );
    expect(stylesheet).toMatch(
      /--apple-danger: #ff453a;[^}]*--apple-danger-hover: #d70015;[^}]*--apple-danger-soft: rgba\(255, 69, 58, 0\.13\);[^}]*--apple-danger-hover-soft: rgba\(255, 69, 58, 0\.21\);[^}]*--apple-danger-border: rgba\(255, 69, 58, 0\.38\);[^}]*--apple-error: var\(--apple-danger\);/s,
    );
    expect(stylesheet).not.toMatch(/#3277a8|#327b70|rgba\(50,\s*119,\s*168/);
  });

  it("keeps settings on the shared management-center palette", () => {
    expect(stylesheet).toMatch(
      /\.app-shell--spotlight \.settings-panel \{[^}]*background: var\(--apple-surface\);[^}]*border-color: var\(--apple-border\);[^}]*color: var\(--apple-text\);[^}]*color-scheme: light;/s,
    );
    expect(stylesheet).toMatch(
      /\.app-shell--spotlight \.settings-switch \{[^}]*background: var\(--apple-active\);/s,
    );
    expect(stylesheet).toMatch(
      /\.app-shell--spotlight \.settings-switch\.is-on \{[^}]*background: var\(--apple-primary\);[^}]*border-color: var\(--apple-primary\);/s,
    );
    expect(stylesheet).toMatch(
      /\.app-shell--spotlight \.settings-action\.is-danger \{[^}]*background: var\(--apple-danger-soft\);[^}]*border-color: var\(--apple-danger-border\);[^}]*color: var\(--apple-danger\);/s,
    );
    expect(stylesheet).toMatch(
      /\.app-shell--spotlight \.settings-action\.is-danger:hover:not\(:disabled\) \{[^}]*background: var\(--apple-danger-hover-soft\);[^}]*border-color: var\(--apple-danger\);[^}]*color: var\(--apple-danger-hover\);/s,
    );
    expect(stylesheet).toMatch(
      /\.settings-hotkey-recorder:focus-visible,[^{]*\{[^}]*outline: 2px solid var\(--apple-primary\);/s,
    );
    expect(stylesheet).toMatch(
      /\.settings-error \{[^}]*color: var\(--apple-error\) !important;/s,
    );
  });

  it("keeps the built-in workbench on the shared light product palette", () => {
    expect(stylesheet).toMatch(
      /\.toolbox-drawer \{[^}]*background: var\(--apple-surface\);[^}]*border-color: var\(--apple-border\);[^}]*color: var\(--apple-text\);[^}]*color-scheme: light;/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-tab\.is-active \{[^}]*background: var\(--apple-active\);[^}]*border-color: var\(--apple-active\);[^}]*color: var\(--apple-text\);/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-code-input,\s*\.toolbox-calculator-input,\s*\.toolbox-field input \{[^}]*background: var\(--apple-card\);[^}]*border-color: var\(--apple-border\);[^}]*color: var\(--apple-text\);/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-primary-action,\s*\.toolbox-record-action,\s*\.toolbox-drawer \.accent-button \{[^}]*background: var\(--apple-primary\);[^}]*border: 1px solid var\(--apple-primary\);/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-danger-action \{[^}]*background: var\(--apple-danger-soft\);[^}]*border-color: var\(--apple-danger-border\);[^}]*color: var\(--apple-danger\);/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-danger-action:hover:not\(:disabled\) \{[^}]*background: var\(--apple-danger-hover-soft\);[^}]*border-color: var\(--apple-danger\);[^}]*color: var\(--apple-danger-hover\);/s,
    );
  });

  it("keeps embedded and detached plugin chrome on the same light palette", () => {
    expect(stylesheet).toMatch(
      /\.plugin-frame-overlay \{[^}]*background: var\(--apple-surface\);[^}]*border-color: var\(--apple-border\);[^}]*color: var\(--apple-text\);[^}]*color-scheme: light;/s,
    );
    expect(stylesheet).toMatch(
      /\.plugin-frame__header \{[^}]*background: var\(--apple-surface\);[^}]*border-color: var\(--apple-border\);/s,
    );
    expect(stylesheet).toMatch(
      /\.plugin-frame__tag,\s*\.plugin-frame__detach \{[^}]*background: var\(--apple-card\);[^}]*border-color: var\(--apple-border\);[^}]*color: var\(--apple-text\);/s,
    );
    expect(stylesheet).toMatch(
      /\.plugin-frame__detached-preview \{[^}]*background: var\(--apple-surface\);/s,
    );
  });

  it("preserves readable semantic state colors on light backgrounds", () => {
    expect(stylesheet).toMatch(
      /\.toolbox-feedback\.is-success \{[^}]*color: var\(--apple-success\);/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-feedback\.is-warning \{[^}]*color: var\(--apple-warning\);/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-feedback\.is-error \{[^}]*color: var\(--apple-error\);/s,
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
      /\.settings-log__entry\.is-error \.settings-log__level \{[^}]*color: var\(--apple-error\);/s,
    );
  });
});
