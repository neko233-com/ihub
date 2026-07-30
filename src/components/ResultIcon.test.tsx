import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { ResultIcon } from "./ResultIcon";

const onePixelPng = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZrG8AAAAASUVORK5CYII=";

describe("ResultIcon", () => {
  it("reserves a neutral application placeholder only while native artwork is pending", () => {
    const pendingMarkup = renderToStaticMarkup(
      <ResultIcon kind="application" nativeIconPending />,
    );
    const settledMarkup = renderToStaticMarkup(<ResultIcon kind="application" />);

    expect(pendingMarkup).toContain("is-loading-native");
    expect(pendingMarkup).not.toContain("<svg");
    expect(pendingMarkup).not.toContain("<img");
    expect(settledMarkup).not.toContain("is-loading-native");
    expect(settledMarkup).toContain("<svg");
  });

  it("renders validated native artwork when it is available", () => {
    const markup = renderToStaticMarkup(
      <ResultIcon iconSrc={onePixelPng} kind="application" />,
    );

    expect(markup).toContain("is-native");
    expect(markup).toContain("<img");
    expect(markup).not.toContain("<svg");
  });

  it("can reserve the same native slot for an indexed file without a fake flash", () => {
    const pendingMarkup = renderToStaticMarkup(
      <ResultIcon kind="file" nativeIconPending />,
    );
    const settledMarkup = renderToStaticMarkup(<ResultIcon kind="file" />);

    expect(pendingMarkup).toContain("is-loading-native");
    expect(pendingMarkup).not.toContain("<svg");
    expect(settledMarkup).toContain("<svg");
  });

  it("retains vector fallbacks for non-native result kinds", () => {
    const markup = renderToStaticMarkup(<ResultIcon kind="plugin" />);

    expect(markup).toContain("<svg");
    expect(markup).not.toContain("is-loading-native");
  });
});
