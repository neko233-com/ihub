import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  createSpotlightNativeIconPendingBatch,
  settleSpotlightNativeIconPendingBatch,
  SpotlightLauncher,
} from "./SpotlightLauncher";

const onePixelPng = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAusB9Y9ZrG8AAAAASUVORK5CYII=";

function renderedSearchIcon(item: {
  iconSrc?: string;
  nativeIconPending?: boolean;
}) {
  const markup = renderToStaticMarkup(
    <SpotlightLauncher
      onClose={() => undefined}
      open
      query="app"
      searchResults={[{
        id: "application:test",
        label: "Test App",
        ...item,
      }]}
    />,
  );
  const match = markup.match(
    /<span aria-hidden="true" class="(ihub-spotlight__result-icon[^"]*)">([\s\S]*?)<\/span>/,
  );
  expect(match).not.toBeNull();
  return {
    className: match?.[1] ?? "",
    contents: match?.[2] ?? "",
  };
}

describe("SpotlightLauncher uTools palette", () => {
  it("uses the bright uTools palette regardless of the system color scheme", () => {
    const markup = renderToStaticMarkup(
      <SpotlightLauncher
        onClose={() => undefined}
        open
      />,
    );

    expect(markup).toContain("--ihub-utools-surface: #f4f4f4");
    expect(markup).toContain("--ihub-utools-selected: #d7d7d7");
    expect(markup).toContain("--ihub-utools-hover: rgba(0, 0, 0, .04)");
    expect(markup).toContain("background: var(--ihub-utools-surface)");
    expect(markup).toContain("color-scheme: light");
    expect(markup).not.toContain("--ihub-utools-surface: #303133");
    expect(markup).not.toContain("@media (prefers-color-scheme: light)");
  });

  it("shows a neutral application glyph after a native icon request settles empty", () => {
    const pending = renderedSearchIcon({ nativeIconPending: true });
    const settled = renderedSearchIcon({ nativeIconPending: false });

    expect(pending.className).toContain("is-loading-native");
    expect(pending.contents).not.toContain("<svg");
    expect(settled.className).not.toContain("is-loading-native");
    expect(settled.contents).toContain("<svg");
  });

  it("keeps successful native PNG artwork after the pending batch settles", () => {
    const icon = renderedSearchIcon({
      iconSrc: onePixelPng,
      nativeIconPending: true,
    });

    expect(icon.className).toContain("is-native");
    expect(icon.className).not.toContain("is-loading-native");
    expect(icon.contents).toContain("<img");
    expect(icon.contents).not.toContain("<svg");
  });

  it("settles only the matching native icon request generation", () => {
    const batch = createSpotlightNativeIconPendingBatch(
      7,
      ["application:test"],
      ["shortcut:test"],
    );

    expect(batch?.searchResultIds.has("application:test")).toBe(true);
    expect(batch?.launcherShortcutIds.has("shortcut:test")).toBe(true);
    expect(settleSpotlightNativeIconPendingBatch(batch, 6)).toBe(batch);
    expect(settleSpotlightNativeIconPendingBatch(batch, 7)).toBeNull();
    expect(createSpotlightNativeIconPendingBatch(8, [], [])).toBeNull();
  });
});
