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
  it("uses the uTools 7.8 light palette and two-pane geometry", () => {
    const markup = renderCenter();

    expect(markup).toMatch(
      /\.plugin-center \{[^}]*background: #f4f4f4;[^}]*border: 1px solid #d0d0d0;[^}]*color: rgba\(0, 0, 0, \.87\);[^}]*color-scheme: light;[^}]*font-family: system-ui,/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__body \{[^}]*grid-template-columns: 220px minmax\(0, 1fr\);/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__sidebar \{[^}]*background: #f4f4f4;[^}]*border-right: 1px solid #d0d0d0;[^}]*padding: 0;/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__page-body \{[^}]*max-width: 800px;[^}]*padding: 12px 20px 23px;/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__context-confirm-card \{[^}]*background: #fff;[^}]*border: 1px solid #d0d0d0;/s,
    );
    expect(markup).toContain("color: #3f51b5");
    expect(markup).not.toContain("#1677ff");
    expect(markup).not.toContain('"DM Mono"');
  });

  it("matches the compact installed and two-column market item metrics", () => {
    const markup = renderCenter();

    expect(markup).toMatch(
      /\.plugin-center__side-heading \{[^}]*color: #515151;[^}]*font-size: 13px;[^}]*height: 40px;[^}]*padding: 0 14px 0 18px;/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__installed \{[^}]*padding: 0 8px 8px;/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__installed-item \{[^}]*border-radius: 5px;[^}]*min-height: 42px;[^}]*padding: 8px 6px 8px 12px;/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__installed-item\.is-selected \{ background: #d7d7d7; \}/,
    );
    expect(markup).toMatch(
      /\.plugin-center__installed-item:hover \{ background: rgba\(0, 0, 0, \.04\); \}/,
    );
    expect(markup).toMatch(
      /\.plugin-center__installed-icon \{[^}]*height: 26px;[^}]*width: 26px;/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__market-grid \{[^}]*background: #fff;[^}]*border-radius: 7px;[^}]*gap: 20px;[^}]*grid-template-columns: repeat\(2, minmax\(0, 1fr\)\);[^}]*padding: 12px;/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__market-item \{[^}]*border-bottom: 1px solid #e6e6e6;[^}]*grid-template-columns: 42px minmax\(0, 1fr\) auto;/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__market-icon \{[^}]*height: 42px;[^}]*width: 42px;/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__market-copy p \{[^}]*color: #737373;[^}]*font-size: 12px;/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__market-action \{[^}]*background: rgba\(0, 0, 0, \.08\);[^}]*border-radius: 10px;[^}]*height: 20px;[^}]*min-width: 42px;/s,
    );
    expect(markup).toMatch(
      /\.plugin-center__market-action:hover:not\(:disabled\) \{ background: rgba\(0, 0, 0, \.16\); color: #212121; \}/,
    );
    expect(markup).toMatch(
      /\.plugin-center__market-actions \{[^}]*flex-wrap: wrap;[^}]*max-width: 104px;[^}]*min-width: 0;/s,
    );
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
      /\.plugin-center__topbar \{[^}]*grid-template-columns: 220px minmax\(127px, 1fr\) auto;[^}]*min-height: 48px;/s,
    );
    expect(markup).not.toContain(".plugin-center__drag-zone::before");
  });
});
