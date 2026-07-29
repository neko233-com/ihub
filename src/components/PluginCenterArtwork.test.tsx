import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { PluginInfo } from "../lib/types";
import { PluginCenter } from "./PluginCenter";

const onePixelPng = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZrG8AAAAASUVORK5CYII=";

function renderCenter(plugin: PluginInfo) {
  return renderToStaticMarkup(
    <PluginCenter
      onClose={() => undefined}
      onPluginsChanged={() => undefined}
      onToast={() => undefined}
      open
      plugins={[plugin]}
    />,
  );
}

describe("PluginCenter installed artwork", () => {
  it("uses validated installed artwork while retaining catalog glyph fallbacks", () => {
    const artworkMarkup = renderCenter({
      iconSrc: onePixelPng,
      id: "ihub-plugin-ocr",
      name: "OCR 文字识别",
      version: "1.0.0",
    });
    const unsafeMarkup = renderCenter({
      iconSrc: "file:///C:/plugins/ocr/icon.png",
      id: "ihub-plugin-ocr",
      name: "OCR 文字识别",
      version: "1.0.0",
    });

    expect(artworkMarkup).toContain("plugin-center__installed-icon--ocr is-artwork");
    expect(artworkMarkup).toContain("plugin-center__market-icon--ocr is-artwork");
    expect(artworkMarkup).toContain(`<img alt="" draggable="false" src="${onePixelPng}"/>`);
    expect(unsafeMarkup).not.toContain("plugin-center__installed-icon--ocr is-artwork");
    expect(unsafeMarkup).not.toContain("plugin-center__market-icon--ocr is-artwork");
    expect(unsafeMarkup).not.toContain("file:///C:/plugins/ocr/icon.png");
    expect(unsafeMarkup).toContain("<svg");
  });
});
