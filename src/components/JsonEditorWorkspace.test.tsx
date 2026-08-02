import { readFileSync } from "node:fs";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { JsonEditorWorkspace } from "./JsonEditorWorkspace";

describe("JSON editor reference surface", () => {
  it("renders one full-canvas editor with the reference tabs and bottom query rail", () => {
    const markup = renderToStaticMarkup(
      <JsonEditorWorkspace
        input={'{\n  "key": "value"\n}'}
        onClose={() => undefined}
        onCopy={() => undefined}
        onInputChange={() => undefined}
        onToast={() => undefined}
      />,
    );

    expect(markup).toContain("JSON 编辑器");
    expect(markup).toContain(">Json<");
    expect(markup).toContain('class="json-code-editor"');
    expect(markup).toContain('id="json-workbench-query"');
    expect(markup).toContain('aria-label="JSON 转换工具"');
    expect(markup).not.toContain("JSON 输出");
    expect(markup).not.toContain("json-editor-workspace__panes");
  });

  it("uses CodeMirror and the saturated deep editor material", () => {
    const component = readFileSync(new URL("./JsonEditorWorkspace.tsx", import.meta.url), "utf8");
    const stylesheet = readFileSync(new URL("../index.css", import.meta.url), "utf8");

    expect(component).toContain('from "codemirror"');
    expect(component).toContain('from "@codemirror/lang-json"');
    expect(component).toContain("foldAll(editorViewRef.current)");
    expect(component).toContain("unfoldAll(editorViewRef.current)");
    expect(stylesheet).toMatch(/\.toolbox-drawer\.toolbox-drawer--json \{[^}]*background: #191b1f;/s);
    expect(stylesheet).toMatch(/\.json-editor-workspace \{[^}]*grid-template-rows: 68px minmax\(0, 1fr\) 58px;/s);
    expect(stylesheet).toContain("#0a84ff");
    expect(stylesheet).toContain("#64d2ff");
  });
});
