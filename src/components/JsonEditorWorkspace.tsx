import { json } from "@codemirror/lang-json";
import {
  HighlightStyle,
  foldAll,
  syntaxHighlighting,
  unfoldAll,
} from "@codemirror/language";
import { tags } from "@lezer/highlight";
import { basicSetup, EditorView } from "codemirror";
import {
  Braces,
  ChevronsDown,
  ChevronsUp,
  CircleAlert,
  Code2,
  Copy,
  EllipsisVertical,
  FileCode2,
  Minimize2,
  Quote,
  Trash2,
  X,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState, type KeyboardEvent } from "react";
import {
  escapeJsonValue,
  formatJsonValue,
  jsonValueToTypeScript,
  jsonValueToXml,
  minifyJsonValue,
  parseStructuredInput,
  queryJsonPath,
  type StructuredInputKind,
} from "../lib/json-workbench";

interface JsonEditorWorkspaceProps {
  input: string;
  onClose: () => void;
  onCopy: (value: string, label: string) => Promise<void> | void;
  onInputChange: (value: string) => void;
  onStartWindowDrag?: () => void;
  onToast: (message: string) => void;
}

interface EditorStatus {
  kind: "success" | "error" | "idle";
  text: string;
}

const inputKindLabels: Record<StructuredInputKind, string> = {
  json: "JSON",
  "url-params": "URL Params",
  xml: "XML",
  yaml: "YAML",
};

const jsonHighlightStyle = HighlightStyle.define([
  { tag: tags.propertyName, color: "#64d2ff" },
  { tag: tags.string, color: "#ff9f7a" },
  { tag: [tags.number, tags.bool], color: "#bf5af2" },
  { tag: tags.null, color: "#ff375f" },
  { tag: tags.invalid, color: "#ff375f", textDecoration: "underline wavy" },
]);

const jsonEditorTheme = EditorView.theme({
  "&": {
    backgroundColor: "#191b1f",
    color: "#f4f6fa",
    height: "100%",
  },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": {
    fontFamily: '"SFMono-Regular", "Cascadia Code", "JetBrains Mono", Consolas, monospace',
    fontSize: "13px",
    lineHeight: "1.65",
    overflow: "auto",
  },
  ".cm-content": {
    caretColor: "#64d2ff",
    minHeight: "100%",
    padding: "8px 0 46px",
  },
  ".cm-line": { padding: "0 15px 0 8px" },
  ".cm-cursor, .cm-dropCursor": { borderLeftColor: "#64d2ff" },
  ".cm-selectionBackground, &.cm-focused .cm-selectionBackground, ::selection": {
    backgroundColor: "rgba(10, 132, 255, 0.34) !important",
  },
  ".cm-activeLine": {
    backgroundColor: "rgba(10, 132, 255, 0.065)",
    boxShadow: "inset 0 1px rgba(100, 210, 255, 0.055), inset 0 -1px rgba(100, 210, 255, 0.055)",
  },
  ".cm-gutters": {
    backgroundColor: "#1c1e22",
    border: "none",
    color: "#8d98a8",
    paddingLeft: "7px",
  },
  ".cm-activeLineGutter": {
    backgroundColor: "rgba(10, 132, 255, 0.1)",
    color: "#d8edff",
  },
  ".cm-lineNumbers .cm-gutterElement": {
    minWidth: "34px",
    padding: "0 10px 0 4px",
  },
  ".cm-foldGutter .cm-gutterElement": {
    color: "#b8c5d5",
    padding: "0 3px",
  },
  ".cm-foldPlaceholder": {
    backgroundColor: "rgba(94, 92, 230, 0.2)",
    border: "1px solid rgba(100, 210, 255, 0.34)",
    color: "#d8edff",
  },
  ".cm-panels": {
    backgroundColor: "#2c2f35",
    color: "#f4f6fa",
  },
  ".cm-searchMatch": { backgroundColor: "rgba(255, 159, 10, 0.32)" },
  ".cm-searchMatch.cm-searchMatch-selected": { backgroundColor: "rgba(10, 132, 255, 0.42)" },
}, { dark: true });

export function JsonEditorWorkspace({
  input,
  onClose,
  onCopy,
  onInputChange,
  onStartWindowDrag,
  onToast,
}: JsonEditorWorkspaceProps) {
  const [query, setQuery] = useState("$");
  const [status, setStatus] = useState<EditorStatus>({
    kind: "idle",
    text: "支持 JSON、URL Params、XML 与 YAML",
  });
  const editorHostRef = useRef<HTMLDivElement>(null);
  const editorViewRef = useRef<EditorView | null>(null);
  const inputRef = useRef(input);
  const callbacksRef = useRef({ onInputChange, onToast });
  const synchronizingRef = useRef(false);

  callbacksRef.current = { onInputChange, onToast };

  const parsed = useMemo(() => {
    try {
      return { result: parseStructuredInput(input), error: null };
    } catch (error) {
      return { result: null, error: error instanceof Error ? error.message : "无法解析输入。" };
    }
  }, [input]);

  useEffect(() => {
    if (!editorHostRef.current) return;

    const view = new EditorView({
      doc: inputRef.current,
      extensions: [
        basicSetup,
        json(),
        syntaxHighlighting(jsonHighlightStyle),
        jsonEditorTheme,
        EditorView.updateListener.of((update) => {
          if (!update.docChanged) return;
          const value = update.state.doc.toString();
          inputRef.current = value;
          if (!synchronizingRef.current) {
            callbacksRef.current.onInputChange(value);
          }
        }),
        EditorView.domEventHandlers({
          paste(event, activeView) {
            const pasted = event.clipboardData?.getData("text/plain") ?? "";
            if (!pasted.trim()) return false;
            try {
              const converted = parseStructuredInput(pasted);
              if (converted.kind === "json") return false;
              event.preventDefault();
              const formatted = formatJsonValue(converted.value);
              activeView.dispatch(activeView.state.replaceSelection(formatted));
              const message = `${inputKindLabels[converted.kind]} 已自动转换为 JSON。`;
              setStatus({ kind: "success", text: message });
              callbacksRef.current.onToast(message);
              return true;
            } catch {
              return false;
            }
          },
        }),
      ],
      parent: editorHostRef.current,
    });
    editorViewRef.current = view;
    return () => {
      editorViewRef.current = null;
      view.destroy();
    };
  }, []);

  useEffect(() => {
    const view = editorViewRef.current;
    if (!view || view.state.doc.toString() === input) {
      inputRef.current = input;
      return;
    }
    synchronizingRef.current = true;
    view.dispatch({ changes: { from: 0, to: view.state.doc.length, insert: input } });
    synchronizingRef.current = false;
    inputRef.current = input;
  }, [input]);

  useEffect(() => {
    if (parsed.result) {
      setStatus({
        kind: "success",
        text: `${inputKindLabels[parsed.result.kind]} 有效 · 本地离线`,
      });
    } else {
      setStatus({
        kind: input.trim() ? "error" : "idle",
        text: parsed.error ?? "等待输入",
      });
    }
  }, [input, parsed]);

  const replaceEditorDocument = (value: string, message: string) => {
    const view = editorViewRef.current;
    if (view) {
      view.dispatch({
        changes: { from: 0, to: view.state.doc.length, insert: value },
        selection: { anchor: Math.min(value.length, view.state.selection.main.head) },
        scrollIntoView: true,
      });
      view.focus();
    } else {
      onInputChange(value);
    }
    setStatus({ kind: "success", text: message });
    onToast(message);
  };

  const requireParsed = () => {
    if (parsed.result) return parsed.result;
    const message = parsed.error ?? "无法解析输入。";
    setStatus({ kind: "error", text: message });
    onToast(message);
    return null;
  };

  const formatEditor = () => {
    const source = requireParsed();
    if (!source) return;
    replaceEditorDocument(
      formatJsonValue(source.value),
      `${inputKindLabels[source.kind]} 已格式化为 JSON。`,
    );
  };

  const minifyEditor = () => {
    const source = requireParsed();
    if (!source) return;
    replaceEditorDocument(minifyJsonValue(source.value), "JSON 已压缩，可用 Ctrl/⌘ + Z 撤销。");
  };

  const escapeEditor = () => {
    const source = requireParsed();
    if (!source) return;
    replaceEditorDocument(escapeJsonValue(source.value), "JSON 已转义，可用 Ctrl/⌘ + Z 撤销。");
  };

  const copyConverted = (kind: "xml" | "typescript") => {
    const source = requireParsed();
    if (!source) return;
    const value = kind === "xml" ? jsonValueToXml(source.value) : jsonValueToTypeScript(source.value);
    const label = kind === "xml" ? "XML" : "TypeScript";
    void onCopy(value, label);
    setStatus({ kind: "success", text: `${label} 已复制，原 JSON 保持不变。` });
  };

  const runQuery = () => {
    const source = requireParsed();
    if (!source) return;
    try {
      const result = queryJsonPath(source.value, query);
      replaceEditorDocument(
        result.formatted,
        `查询完成，返回 ${result.matches.length} 项；可用 Ctrl/⌘ + Z 撤销。`,
      );
    } catch (error) {
      const message = error instanceof Error ? error.message : "无法执行查询。";
      setStatus({ kind: "error", text: message });
      onToast(message);
    }
  };

  const handleWorkspaceKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (!(event.ctrlKey || event.metaKey)) return;
    if (event.key.toLowerCase() === "l") {
      event.preventDefault();
      formatEditor();
    } else if (event.key === "Enter") {
      event.preventDefault();
      runQuery();
    }
  };

  const clearEditor = () => {
    replaceEditorDocument("", "编辑器已清空，可用 Ctrl/⌘ + Z 撤销。");
  };

  return (
    <section
      aria-labelledby="json-editor-title"
      className="json-editor-workspace"
      id="toolbox-panel-json"
      onKeyDownCapture={handleWorkspaceKeyDown}
      role="tabpanel"
    >
      <header className="json-editor-workspace__header" onPointerDown={onStartWindowDrag}>
        <div className="json-editor-workspace__tabs">
          <div className="json-editor-workspace__tab is-product">
            <span className="json-editor-workspace__tab-icon"><Braces size={12} /></span>
            <h2 id="json-editor-title">JSON 编辑器</h2>
          </div>
          <div className="json-editor-workspace__tab is-document">
            <span>Json</span>
            <button
              aria-label="关闭 JSON 编辑器"
              onClick={onClose}
              onPointerDown={(event) => event.stopPropagation()}
              type="button"
            >
              <X size={20} />
            </button>
          </div>
        </div>

        <div className="json-editor-workspace__window-actions" onPointerDown={(event) => event.stopPropagation()}>
          <details className="json-editor-workspace__menu">
            <summary aria-label="JSON 编辑器菜单" title="JSON 编辑器菜单">
              <EllipsisVertical size={21} />
            </summary>
            <div role="menu">
              <button onClick={formatEditor} role="menuitem" type="button">格式化 JSON</button>
              <button onClick={() => void onCopy(input, "JSON")} role="menuitem" type="button">复制当前内容</button>
              <button className="is-danger" onClick={clearEditor} role="menuitem" type="button">清空编辑器</button>
            </div>
          </details>
          <span
            className={`json-editor-workspace__app-mark is-${status.kind}`}
            title={status.text}
          >
            <Braces size={14} />
            <strong>JSON</strong>
          </span>
        </div>
      </header>

      <div className="json-editor-workspace__canvas">
        <div
          aria-label="JSON 输入"
          className="json-code-editor"
          ref={editorHostRef}
        />
        {status.kind === "error" ? (
          <div className="json-editor-workspace__diagnostic" role="status">
            <CircleAlert size={14} />
            <span>{status.text}</span>
          </div>
        ) : null}
      </div>

      <footer className="json-editor-workspace__footer">
        <label className="json-query-bar" htmlFor="json-workbench-query">
          <span>this</span>
          <input
            id="json-workbench-query"
            onChange={(event) => setQuery(event.target.value)}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                runQuery();
              }
            }}
            placeholder={'JS 过滤；示例 ".key.subkey"、"[0][1]"、".map(x=>x.val)"'}
            spellCheck="false"
            value={query}
          />
        </label>
        <div className="json-editor-workspace__tools" aria-label="JSON 转换工具">
          <button onClick={formatEditor} title="格式化 JSON（Ctrl/⌘ + L）" type="button"><Braces size={21} /></button>
          <button onClick={() => editorViewRef.current && foldAll(editorViewRef.current)} title="全部折叠" type="button"><ChevronsUp size={21} /></button>
          <button onClick={() => editorViewRef.current && unfoldAll(editorViewRef.current)} title="全部展开" type="button"><ChevronsDown size={21} /></button>
          <button onClick={minifyEditor} title="压缩 JSON" type="button"><Minimize2 size={21} /></button>
          <button onClick={escapeEditor} title="转义 JSON" type="button"><Quote size={21} /></button>
          <button onClick={() => copyConverted("xml")} title="复制为 XML" type="button"><Code2 size={21} /></button>
          <button onClick={() => copyConverted("typescript")} title="复制为 TypeScript" type="button"><FileCode2 size={21} /></button>
          <button disabled={!input} onClick={() => void onCopy(input, "JSON")} title="复制当前内容" type="button"><Copy size={21} /></button>
          <button className="is-danger" onClick={clearEditor} title="清空编辑器" type="button"><Trash2 size={20} /></button>
        </div>
      </footer>
    </section>
  );
}
