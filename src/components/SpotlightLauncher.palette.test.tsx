import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { SpotlightLauncher } from "./SpotlightLauncher";

describe("SpotlightLauncher uTools palette", () => {
  it("follows the measured uTools dark palette with a light system override", () => {
    const markup = renderToStaticMarkup(
      <SpotlightLauncher
        onClose={() => undefined}
        open
      />,
    );

    expect(markup).toContain("--ihub-utools-surface: #303133");
    expect(markup).toContain("--ihub-utools-selected: #575757");
    expect(markup).toContain("--ihub-utools-hover: rgba(255, 255, 255, .05)");
    expect(markup).toContain("background: var(--ihub-utools-surface)");
    expect(markup).toContain("color-scheme: dark");
    expect(markup).toContain("@media (prefers-color-scheme: light)");
    expect(markup).toContain("--ihub-utools-surface: #f4f4f4");
    expect(markup).toContain("--ihub-utools-selected: #d7d7d7");
  });
});
