import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { WINDOW_DRAG_LONG_PRESS_MS } from "../lib/window-drag-long-press";
import { SpotlightLauncher } from "./SpotlightLauncher";

describe("SpotlightLauncher window drag handle", () => {
  it("reserves an invisible top-edge handle outside the search field", () => {
    const markup = renderToStaticMarkup(
      <SpotlightLauncher
        onClose={() => undefined}
        onStartWindowDrag={() => undefined}
        open
      />,
    );

    const handleIndex = markup.indexOf('data-window-drag-handle=""');
    const searchRowIndex = markup.indexOf('class="ihub-spotlight__search-row"');

    expect(handleIndex).toBeGreaterThan(-1);
    expect(searchRowIndex).toBeGreaterThan(handleIndex);
    expect(markup).toContain(`data-drag-long-press-ms="${WINDOW_DRAG_LONG_PRESS_MS}"`);
    expect(markup).toContain(`title="长按 ${WINDOW_DRAG_LONG_PRESS_MS} 毫秒后拖动窗口"`);
    expect(markup).toMatch(
      /\.ihub-spotlight__drag-zone \{[^}]*height: 10px;[^}]*top: 0;[^}]*width: min\(160px, 24vw\);[^}]*\}/s,
    );
    expect(markup).not.toContain(".ihub-spotlight__drag-zone::before");
  });
});
