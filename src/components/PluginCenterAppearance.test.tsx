import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { WINDOW_DRAG_LONG_PRESS_MS } from "../lib/window-drag-long-press";
import { PluginCenter } from "./PluginCenter";

function renderCenter() {
  return renderToStaticMarkup(
    <PluginCenter
      onClose={() => undefined}
      onPluginsChanged={() => undefined}
      onStartWindowDrag={() => undefined}
      onToast={() => undefined}
      open
      plugins={[]}
    />,
  );
}

describe("PluginCenter uTools visual contract", () => {
  it("uses the restrained light center palette without changing its workspace layout", () => {
    const markup = renderCenter();

    expect(markup).toMatch(
      /\.plugin-center \{[^}]*background: #f4f4f4;[^}]*color: #262626;[^}]*color-scheme: light;/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__sidebar \{[^}]*background: #f4f4f4;[^}]*border-right: 1px solid #dedede;/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__main \{[^}]*background: #f4f4f4;/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__market-item \{[^}]*background: #fff;[^}]*border-bottom: 1px solid #ededed;/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__context-confirm-card \{[^}]*background: #fff;[^}]*border: 1px solid #d9d9d9;/s,
    );
    expect(markup).toContain("color: #1677ff");
  });

  it("reserves the shared invisible 280ms top-edge drag handle above the input", () => {
    const markup = renderCenter();
    const handleIndex = markup.indexOf('data-window-drag-handle=""');
    const searchIndex = markup.lastIndexOf('class="plugin-center__search"');

    expect(handleIndex).toBeGreaterThan(-1);
    expect(searchIndex).toBeGreaterThan(handleIndex);
    expect(markup).toContain(`data-drag-long-press-ms="${WINDOW_DRAG_LONG_PRESS_MS}"`);
    expect(markup).toContain(`title="长按 ${WINDOW_DRAG_LONG_PRESS_MS} 毫秒后拖动窗口"`);
    expect(markup).toMatch(
      /\.plugin-center__drag-zone \{[^}]*height: 10px;[^}]*top: 0;[^}]*width: min\(160px, 24vw\);[^}]*\}/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__topbar \{[^}]*min-height: 58px;/s,
    );
    expect(markup).not.toContain(".plugin-center__drag-zone::before");
  });
});
