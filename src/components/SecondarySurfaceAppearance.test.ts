import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const stylesheet = readFileSync(new URL("../index.css", import.meta.url), "utf8");

describe("secondary surfaces uTools light appearance", () => {
  it("keeps the built-in workbench on the shared light product palette", () => {
    expect(stylesheet).toMatch(
      /\.toolbox-drawer \{[^}]*--ihub-surface: #f4f4f4;[^}]*--ihub-surface-raised: #ffffff;[^}]*--ihub-border: #d2d2d2;[^}]*--ihub-text: #292929;[^}]*background: #f4f4f4;[^}]*color-scheme: light;/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-tab\.is-active \{[^}]*background: #d7d7d7;[^}]*color: #292929;/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-code-input,\s*\.toolbox-calculator-input,\s*\.toolbox-field input \{[^}]*background: #ffffff;[^}]*border-color: #d2d2d2;[^}]*color: #292929;/s,
    );
    expect(stylesheet).toMatch(
      /\.region-capture-editor__workspace \{[^}]*background: #ececec;[^}]*border-color: #d2d2d2;/s,
    );
  });

  it("keeps embedded and detached plugin chrome on the same light palette", () => {
    expect(stylesheet).toMatch(
      /\.plugin-frame-overlay \{[^}]*background: #f4f4f4;[^}]*border-color: #d2d2d2;[^}]*color: #292929;[^}]*color-scheme: light;/s,
    );
    expect(stylesheet).toMatch(
      /\.plugin-frame__header \{[^}]*background: #f4f4f4;[^}]*border-color: #d2d2d2;/s,
    );
    expect(stylesheet).toMatch(
      /\.plugin-frame__tag,\s*\.plugin-frame__detach \{[^}]*background: #ffffff;[^}]*border-color: #d2d2d2;[^}]*color: #292929;/s,
    );
    expect(stylesheet).toMatch(
      /\.plugin-frame__detached-preview \{[^}]*background: #f4f4f4;/s,
    );
  });

  it("preserves readable semantic state colors on light backgrounds", () => {
    expect(stylesheet).toMatch(
      /\.toolbox-feedback\.is-success \{[^}]*color: #2f7553;/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-feedback\.is-warning \{[^}]*color: #8a641c;/s,
    );
    expect(stylesheet).toMatch(
      /\.toolbox-feedback\.is-error \{[^}]*color: #a84249;/s,
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
      /\.settings-log__entry\.is-error \.settings-log__level \{[^}]*color: #b33f46;/s,
    );
  });
});
