import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { SpotlightLauncher } from "./SpotlightLauncher";

describe("SpotlightLauncher uTools palette", () => {
  it("keeps the launcher on the measured light neutral surface", () => {
    const markup = renderToStaticMarkup(
      <SpotlightLauncher
        onClose={() => undefined}
        open
      />,
    );

    expect(markup).toContain("--ihub-utools-surface: #f4f4f4");
    expect(markup).toContain("--ihub-utools-active: #d7d7d7");
    expect(markup).toContain("background: var(--ihub-utools-surface)");
    expect(markup).toContain("color-scheme: light");
  });
});
