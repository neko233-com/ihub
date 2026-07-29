import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ResultIcon } from "./ResultIcon";

const onePixelPng = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZrG8AAAAASUVORK5CYII=";

describe("ResultIcon", () => {
  it("reserves a transparent slot instead of flashing a generic application glyph", () => {
    const markup = renderToStaticMarkup(<ResultIcon kind="application" />);

    expect(markup).toContain("is-loading-native");
    expect(markup).not.toContain("<svg");
    expect(markup).not.toContain("<img");
  });

  it("renders validated native artwork when it is available", () => {
    const markup = renderToStaticMarkup(
      <ResultIcon iconSrc={onePixelPng} kind="application" />,
    );

    expect(markup).toContain("is-native");
    expect(markup).toContain("<img");
    expect(markup).not.toContain("<svg");
  });

  it("retains vector fallbacks for non-native result kinds", () => {
    const markup = renderToStaticMarkup(<ResultIcon kind="plugin" />);

    expect(markup).toContain("<svg");
    expect(markup).not.toContain("is-loading-native");
  });
});
