import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { WINDOW_DRAG_LONG_PRESS_MS } from "../lib/window-drag-long-press";
import { SpotlightLauncher } from "./SpotlightLauncher";

describe("SpotlightLauncher window drag handle", () => {
  it("turns the full search header into the 280ms long-press drag surface", () => {
    const markup = renderToStaticMarkup(
      <SpotlightLauncher
        onClose={() => undefined}
        onStartWindowDrag={() => undefined}
        open
      />,
    );

    const handleIndex = markup.indexOf('data-window-drag-handle=""');
    const searchRowIndex = markup.indexOf('class="ihub-spotlight__search-row"');
    const searchInputIndex = markup.indexOf('aria-label="搜索 iHub"');

    expect(handleIndex).toBeGreaterThan(-1);
    expect(handleIndex).toBeGreaterThan(searchRowIndex);
    expect(searchInputIndex).toBeGreaterThan(handleIndex);
    expect(markup).toContain(`data-drag-long-press-ms="${WINDOW_DRAG_LONG_PRESS_MS}"`);
    expect(markup).toContain(`title="长按 ${WINDOW_DRAG_LONG_PRESS_MS} 毫秒后拖动窗口"`);
    expect(markup).toMatch(
      /\.ihub-spotlight__search-row \{[^}]*cursor: grab;[^}]*height: 56px;[^}]*min-height: 56px;/s,
    );
    expect(markup).not.toContain("ihub-spotlight__drag-zone");
  });
});
