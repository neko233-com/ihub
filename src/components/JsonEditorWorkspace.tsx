import {
  Braces,
  Check,
  CircleAlert,
  Code2,
  Copy,
  EllipsisVertical,
  FileCode2,
  Minimize2,
  Quote,
  Search,
  Trash2,
  X,
} from "lucide-react";
import { useEffect, useMemo, useRef, useState, type ClipboardEvent, type KeyboardEvent } from "react";
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

type OutputMode = "preview" | "formatted" | "minified" | "escaped" | "xml" | "typescript" | "query";

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

const outputModeLabels: Record<OutputMode, string> = {
  preview: "格式化预览",
  formatted: "JSON",
  minified: "压缩 JSON",
  escaped: "转义字符串",
  xml: "XML",
  typescript: "TypeScript",
  query: "查询结果",
};

function initialOutput(input: string): string {
  try {
    return formatJsonValue(parseStructuredInput(input).value);
  } catch {
    return "";
  }
}

function lineNumbers(value: string): string {
  const count = Math.max(1, value.split("\n").length);
  return Array.from({ length: count }, (_, index) => index + 1).join("\n");
}

export function JsonEditorWorkspace({
  input,
  onClose,
  onCopy,
  onInputChange,
  onStartWindowDrag,
  onToast,
}: JsonEditorWorkspaceProps) {
  const [output, setOutput] = useState(() => initialOutput(input));
  const [outputMode, setOutputMode] = useState<OutputMode>("preview");
  const [query, setQuery] = useState("$");
  const [status, setStatus] = useState<EditorStatus>({
    kind: "idle",
    text: "支持 JSON、URL Params、XML 与 YAML",
  });
  const inputGutterRef = useRef<HTMLPreElement>(null);
  const outputGutterRef = useRef<HTMLPreElement>(null);
  const parsed = useMemo(() => {
    try {
      return { result: parseStructuredInput(input), error: null };
    } catch (error) {
      return { result: null, error: error instanceof Error ? error.message : "无法解析输入。" };
    }
  }, [input]);

  useEffect(() => {
    if (outputMode !== "preview") return;
    setOutput(parsed.result ? formatJsonValue(parsed.result.value) : "");
    setStatus(parsed.result
      ? { kind: "success", text: `${inputKindLabels[parsed.result.kind]} 已就绪 · 全程离线` }
      : { kind: input.trim() ? "error" : "idle", text: parsed.error ?? "等待输入" });
  }, [input, outputMode, parsed]);

  const commitOutput = (mode: OutputMode, value: string, message: string) => {
    setOutputMode(mode);
    setOutput(value);
    setStatus({ kind: "success", text: message });
    onToast(message);
  };

  const requireParsed = () => {
    if (!parsed.result) {
      const message = parsed.error ?? "无法解析输入。";
      setStatus({ kind: "error", text: message });
      onToast(message);
      return null;
    }
    return parsed.result;
  };

  const applyTransform = (mode: Exclude<OutputMode, "preview" | "query">) => {
    const source = requireParsed();
    if (!source) return;
    if (mode === "formatted") {
      const formatted = formatJsonValue(source.value);
      onInputChange(formatted);
      commitOutput(mode, formatted, `${inputKindLabels[source.kind]} 已转换并格式化为 JSON。`);
    } else if (mode === "minified") {
      commitOutput(mode, minifyJsonValue(source.value), "JSON 已压缩。原输入保持不变。");
    } else if (mode === "escaped") {
      commitOutput(mode, escapeJsonValue(source.value), "JSON 已转换为转义字符串。");
    } else if (mode === "xml") {
      commitOutput(mode, jsonValueToXml(source.value), "JSON 已转换为 XML。");
    } else {
      commitOutput(mode, jsonValueToTypeScript(source.value), "JSON 已转换为 TypeScript 类型。");
    }
  };

  const runQuery = () => {
    const source = requireParsed();
    if (!source) return;
    try {
      const result = queryJsonPath(source.value, query);
      commitOutput("query", result.formatted, `查询完成，返回 ${result.matches.length} 项。`);
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
      applyTransform("formatted");
    } else if (event.key === "Enter") {
      event.preventDefault();
      runQuery();
    }
  };

  const handlePaste = (event: ClipboardEvent<HTMLTextAreaElement>) => {
    const pasted = event.clipboardData.getData("text/plain");
    if (!pasted.trim()) return;
    try {
      const converted = parseStructuredInput(pasted);
      if (converted.kind === "json") return;
      event.preventDefault();
      const formatted = formatJsonValue(converted.value);
      onInputChange(formatted);
      commitOutput("formatted", formatted, `${inputKindLabels[converted.kind]} 已自动转换为 JSON。`);
    } catch {
      // Let the editor accept ordinary text and surface the parser error inline.
    }
  };

  return (
    <section
      aria-labelledby="json-editor-title"
      className="json-editor-workspace"
      id="toolbox-panel-json"
      onKeyDown={handleWorkspaceKeyDown}
      role="tabpanel"
    >
      <header className="json-editor-workspace__header" onPointerDown={onStartWindowDrag}>
        <div className="json-editor-workspace__identity">
          <span className="json-editor-workspace__command"><Braces size={17} /> JSON</span>
          <div>
            <h2 id="json-editor-title">JSON 编辑器</h2>
            <p>{parsed.result ? `${inputKindLabels[parsed.result.kind]} 输入` : "结构化数据工作台"}</p>
          </div>
        </div>
        <div className={`json-editor-workspace__status is-${status.kind}`} role="status">
          {status.kind === "success" ? <Check size={14} /> : status.kind === "error" ? <CircleAlert size={14} /> : null}
          <span>{status.text}</span>
        </div>
        <div className="json-editor-workspace__window-actions">
          <button aria-label="JSON 编辑器菜单" title="转换操作位于底部工具栏" type="button"><EllipsisVertical size={18} /></button>
          <button aria-label="关闭 JSON 编辑器" onClick={onClose} type="button"><X size={18} /></button>
        </div>
      </header>

      <div className="json-editor-workspace__panes">
        <section className="json-editor-pane" aria-labelledby="json-input-title">
          <header>
            <div>
              <strong id="json-input-title">输入</strong>
              <span>{parsed.result ? inputKindLabels[parsed.result.kind] : "等待有效数据"}</span>
            </div>
            <button onClick={() => applyTransform("formatted")} title="格式化（Ctrl/⌘ + L）" type="button"><Braces size={15} /> 格式化</button>
          </header>
          <div className="json-code-editor">
            <pre aria-hidden="true" className="json-code-editor__gutter" ref={inputGutterRef}>{lineNumbers(input)}</pre>
            <textarea
              aria-label="JSON 输入"
              onChange={(event) => {
                setOutputMode("preview");
                onInputChange(event.target.value);
              }}
              onPaste={handlePaste}
              onScroll={(event) => {
                if (inputGutterRef.current) inputGutterRef.current.scrollTop = event.currentTarget.scrollTop;
              }}
              placeholder={'{\n  "name": "iHub"\n}'}
              spellCheck="false"
              value={input}
            />
          </div>
        </section>

        <section className="json-editor-pane json-editor-pane--output" aria-labelledby="json-output-title">
          <header>
            <div>
              <strong id="json-output-title">输出</strong>
              <span>{outputModeLabels[outputMode]}</span>
            </div>
            <button disabled={!output} onClick={() => void onCopy(output, outputModeLabels[outputMode])} title="复制输出" type="button"><Copy size={15} /> 复制</button>
          </header>
          <div className="json-code-editor is-readonly">
            <pre aria-hidden="true" className="json-code-editor__gutter" ref={outputGutterRef}>{lineNumbers(output)}</pre>
            <textarea
              aria-label="JSON 输出"
              onScroll={(event) => {
                if (outputGutterRef.current) outputGutterRef.current.scrollTop = event.currentTarget.scrollTop;
              }}
              placeholder="格式化结果会显示在这里"
              readOnly
              spellCheck="false"
              value={output}
            />
          </div>
        </section>
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
            placeholder="$.items[*].id"
            spellCheck="false"
            value={query}
          />
          <button onClick={runQuery} title="运行受限 JSONPath 查询（Ctrl/⌘ + Enter）" type="button"><Search size={16} /> 查询</button>
        </label>
        <div className="json-editor-workspace__tools" aria-label="JSON 转换工具">
          <button onClick={() => applyTransform("formatted")} title="格式化 JSON" type="button"><Braces size={17} /><span>格式化</span></button>
          <button onClick={() => applyTransform("minified")} title="压缩 JSON" type="button"><Minimize2 size={17} /><span>压缩</span></button>
          <button onClick={() => applyTransform("escaped")} title="JSON 转义" type="button"><Quote size={17} /><span>转义</span></button>
          <button onClick={() => applyTransform("xml")} title="转换为 XML" type="button"><FileCode2 size={17} /><span>XML</span></button>
          <button onClick={() => applyTransform("typescript")} title="转换为 TypeScript" type="button"><Code2 size={17} /><span>TypeScript</span></button>
          <button disabled={!output} onClick={() => void onCopy(output, outputModeLabels[outputMode])} title="复制当前输出" type="button"><Copy size={17} /><span>复制</span></button>
          <button className="is-danger" onClick={() => {
            onInputChange("");
            setOutput("");
            setOutputMode("preview");
            setStatus({ kind: "idle", text: "内容已清空" });
          }} title="清空编辑器" type="button"><Trash2 size={17} /><span>清空</span></button>
        </div>
      </footer>
    </section>
  );
}
