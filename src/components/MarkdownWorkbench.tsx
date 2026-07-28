import {
  BookOpenText,
  Clipboard,
  Download,
  FileInput,
  FileText,
  ListTree,
  Trash2,
} from "lucide-react";
import { motion, useReducedMotion } from "motion/react";
import { useEffect, useMemo, useRef, useState } from "react";
import { command, isDesktop } from "../lib/desktop";
import {
  MAX_MARKDOWN_DOCUMENT_CHARACTERS,
  parseMarkdown,
  summarizeMarkdown,
  type MarkdownBlock,
  type MarkdownInline,
} from "../lib/safe-markdown";

const markdownStorageKey = "ihub.toolbox.markdown-workbench.v1";
const maxImportBytes = 2 * 1024 * 1024;

const starterDocument = `# Markdown 工作台

在左侧写作，右侧即时预览。内容只保存在这台设备的本地存储中，**不会上传**。

## 常用语法

- 用 \`#\` 创建标题
- 用 \`**粗体**\`、\`*斜体*\` 和 \`[链接](https://example.com)\` 标记重点
- 用 \`- [ ]\` 建立任务列表

> 适合临时 README、发布说明和结构化速记。

\`\`\`ts
const launcher = "iHub";
\`\`\`
`;

export interface MarkdownWorkbenchProps {
  onToast: (message: string) => void;
}

function readStoredMarkdown() {
  if (typeof window === "undefined") {
    return starterDocument;
  }
  try {
    const stored = window.localStorage.getItem(markdownStorageKey);
    return typeof stored === "string" && stored.length ? stored.slice(0, MAX_MARKDOWN_DOCUMENT_CHARACTERS) : starterDocument;
  } catch {
    return starterDocument;
  }
}

function filenameFromHeading(value: string | undefined) {
  const normalized = (value ?? "ihub-note")
    .trim()
    .replace(/[\\/:*?\"<>|]+/g, "-")
    .replace(/\s+/g, "-")
    .replace(/-+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 72);
  return `${normalized || "ihub-note"}.md`;
}

function InlineMarkdown({ nodes }: { nodes: MarkdownInline[] }) {
  return (
    <>
      {nodes.map((node, index) => {
        const key = `${node.kind}-${index}`;
        if (node.kind === "text") {
          return <span key={key}>{node.value}</span>;
        }
        if (node.kind === "code") {
          return <code key={key}>{node.value}</code>;
        }
        if (node.kind === "strong") {
          return <strong key={key}><InlineMarkdown nodes={node.children} /></strong>;
        }
        if (node.kind === "emphasis") {
          return <em key={key}><InlineMarkdown nodes={node.children} /></em>;
        }
        return (
          <a href={node.href} key={key} rel="noreferrer" target="_blank">
            <InlineMarkdown nodes={node.children} />
          </a>
        );
      })}
    </>
  );
}

function MarkdownBlockView({
  block,
  headingRefs,
}: {
  block: MarkdownBlock;
  headingRefs: React.MutableRefObject<Record<string, HTMLElement | null>>;
}) {
  if (block.kind === "heading") {
    const headingProps = {
      id: block.heading.id,
      ref: (element: HTMLElement | null) => { headingRefs.current[block.heading.id] = element; },
    };
    const content = <InlineMarkdown nodes={block.heading.inline} />;
    switch (block.heading.level) {
      case 1: return <h1 {...headingProps}>{content}</h1>;
      case 2: return <h2 {...headingProps}>{content}</h2>;
      case 3: return <h3 {...headingProps}>{content}</h3>;
      case 4: return <h4 {...headingProps}>{content}</h4>;
      case 5: return <h5 {...headingProps}>{content}</h5>;
      default: return <h6 {...headingProps}>{content}</h6>;
    }
  }
  if (block.kind === "paragraph") {
    return <p><InlineMarkdown nodes={block.inline} /></p>;
  }
  if (block.kind === "quote") {
    return <blockquote><InlineMarkdown nodes={block.inline} /></blockquote>;
  }
  if (block.kind === "rule") {
    return <hr />;
  }
  if (block.kind === "code") {
    return (
      <pre>
        {block.language ? <span className="markdown-workbench__language">{block.language}</span> : null}
        <code>{block.value}</code>
      </pre>
    );
  }
  if (block.kind === "list") {
    const List = block.ordered ? "ol" : "ul";
    return (
      <List start={block.ordered && block.start !== 1 ? block.start : undefined}>
        {block.items.map((item, index) => (
          <li className={typeof item.checked === "boolean" ? "is-task" : undefined} key={index}>
            {typeof item.checked === "boolean" ? (
              <span aria-label={item.checked ? "已完成" : "未完成"} className={`markdown-workbench__task ${item.checked ? "is-checked" : ""}`} role="img">
                {item.checked ? "✓" : ""}
              </span>
            ) : null}
            <InlineMarkdown nodes={item.inline} />
          </li>
        ))}
      </List>
    );
  }
  return (
    <div className="markdown-workbench__table-wrap">
      <table>
        <thead>
          <tr>{block.headers.map((header, index) => <th key={index}><InlineMarkdown nodes={header} /></th>)}</tr>
        </thead>
        <tbody>
          {block.rows.map((row, rowIndex) => (
            <tr key={rowIndex}>{row.map((cell, cellIndex) => <td key={cellIndex}><InlineMarkdown nodes={cell} /></td>)}</tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

async function writeText(text: string) {
  if (isDesktop()) {
    await command<void>("write_clipboard_text", { text });
    return;
  }
  if (!navigator.clipboard?.writeText) {
    throw new Error("当前浏览器不允许写入剪贴板。");
  }
  await navigator.clipboard.writeText(text);
}

export function MarkdownWorkbench({ onToast }: MarkdownWorkbenchProps) {
  const [source, setSource] = useState(readStoredMarkdown);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const headingRefs = useRef<Record<string, HTMLElement | null>>({});
  const prefersReducedMotion = useReducedMotion();
  const markdownDocument = useMemo(() => parseMarkdown(source), [source]);
  const summary = useMemo(() => summarizeMarkdown(source), [source]);
  const documentTitle = markdownDocument.headings[0]?.text;

  useEffect(() => {
    try {
      window.localStorage.setItem(markdownStorageKey, source);
    } catch {
      // Local persistence is intentionally an enhancement. The current draft
      // stays usable in a private or quota-constrained WebView session.
    }
  }, [source]);

  const exportMarkdown = () => {
    const blob = new Blob([source], { type: "text/markdown;charset=utf-8" });
    const url = URL.createObjectURL(blob);
    const link = window.document.createElement("a");
    link.href = url;
    link.download = filenameFromHeading(documentTitle);
    link.style.display = "none";
    window.document.body.append(link);
    link.click();
    link.remove();
    window.setTimeout(() => URL.revokeObjectURL(url), 0);
    onToast("已导出 Markdown 源文件。");
  };

  const importMarkdown = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    event.target.value = "";
    if (!file) {
      return;
    }
    const allowed = /\.(?:md|markdown|mdown|mkdn|txt)$/i.test(file.name) || file.type.startsWith("text/");
    if (!allowed) {
      onToast("请选择 Markdown 或纯文本文件。");
      return;
    }
    if (file.size > maxImportBytes) {
      onToast("为保持即时预览，导入文件不能超过 2 MiB。");
      return;
    }
    try {
      setSource(await file.text());
      onToast(`已导入 ${file.name}；内容仍只保留在本机。`);
    } catch {
      onToast("无法读取所选文本文件。");
    }
  };

  const clearDocument = () => {
    if (!source || window.confirm("清空当前 Markdown 草稿？本地自动保存的内容也会被替换。")) {
      setSource("");
      onToast("已清空 Markdown 草稿。");
    }
  };

  return (
    <section aria-labelledby="toolbox-markdown-title" id="toolbox-panel-markdown" role="tabpanel">
      <div className="toolbox-section-heading">
        <span className="toolbox-section-heading__icon"><BookOpenText size={17} /></span>
        <div>
          <h3 id="toolbox-markdown-title">Markdown 工作台</h3>
          <p>离线写作、结构导航和安全预览；原始 Markdown 不会当作 HTML 执行。</p>
        </div>
      </div>

      <div className="markdown-workbench__toolbar">
        <span className="markdown-workbench__status">
          <FileText size={13} /> {summary.words} 词 · {summary.characters.toLocaleString()} 字符{summary.readingMinutes ? ` · 约 ${summary.readingMinutes} 分钟` : ""}
        </span>
        <div className="markdown-workbench__actions">
          <input accept=".md,.markdown,.mdown,.mkdn,.txt,text/markdown,text/plain" aria-label="导入 Markdown 文件" hidden onChange={(event) => void importMarkdown(event)} ref={fileInputRef} type="file" />
          <button className="toolbox-icon-action" onClick={() => fileInputRef.current?.click()} title="导入 Markdown 文件" type="button">
            <FileInput size={15} />
          </button>
          <button className="toolbox-icon-action" onClick={() => void writeText(source).then(() => onToast("已复制 Markdown 源文件。"), () => onToast("无法写入剪贴板。"))} title="复制 Markdown 源文件" type="button">
            <Clipboard size={15} />
          </button>
          <button className="toolbox-icon-action" onClick={exportMarkdown} title="导出 Markdown 文件" type="button">
            <Download size={15} />
          </button>
          <button className="toolbox-icon-action" disabled={!source} onClick={clearDocument} title="清空当前草稿" type="button">
            <Trash2 size={15} />
          </button>
        </div>
      </div>

      {markdownDocument.truncated ? (
        <p className="toolbox-feedback is-warning" role="status">为保持流畅，预览仅显示前 {(MAX_MARKDOWN_DOCUMENT_CHARACTERS / 1_000).toFixed(0)}k 字符；导出仍保留完整源文档。</p>
      ) : null}

      <div className="markdown-workbench">
        <label className="markdown-workbench__editor" htmlFor="markdown-workbench-input">
          <span><BookOpenText size={13} /> 源文档</span>
          <textarea
            id="markdown-workbench-input"
            onChange={(event) => setSource(event.target.value.slice(0, MAX_MARKDOWN_DOCUMENT_CHARACTERS))}
            onKeyDown={(event) => {
              if ((event.ctrlKey || event.metaKey) && event.key.toLocaleLowerCase() === "s") {
                event.preventDefault();
                exportMarkdown();
              }
            }}
            placeholder="# 开始写作"
            spellCheck="true"
            value={source}
          />
        </label>

        <motion.div
          className="markdown-workbench__preview-shell"
          initial={prefersReducedMotion ? false : { opacity: 0, y: 5 }}
          animate={{ opacity: 1, y: 0 }}
          transition={{ duration: prefersReducedMotion ? 0 : 0.18, ease: [0.16, 1, 0.3, 1] }}
        >
          <div className="markdown-workbench__preview-heading">
            <span><BookOpenText size={13} /> 预览</span>
            <span>安全渲染</span>
          </div>
          <article className="markdown-workbench__preview" aria-label="Markdown 预览">
            {markdownDocument.blocks.length ? markdownDocument.blocks.map((block, index) => (
              <MarkdownBlockView block={block} headingRefs={headingRefs} key={index} />
            )) : <p className="markdown-workbench__empty">从左侧开始写作，预览会即时出现。</p>}
          </article>
        </motion.div>
      </div>

      {markdownDocument.headings.length ? (
        <nav aria-label="Markdown 目录" className="markdown-workbench__outline">
          <div><ListTree size={14} /><strong>文档目录</strong></div>
          <ol>
            {markdownDocument.headings.map((heading) => (
              <li key={heading.id} style={{ paddingInlineStart: `${Math.max(0, heading.level - 1) * 10}px` }}>
                <button
                  onClick={() => headingRefs.current[heading.id]?.scrollIntoView({ behavior: prefersReducedMotion ? "auto" : "smooth", block: "start" })}
                  type="button"
                >
                  {heading.text || "未命名标题"}
                </button>
              </li>
            ))}
          </ol>
        </nav>
      ) : null}
      <p className="toolbox-note">支持标题、段落、引用、任务列表、代码块、表格、粗体、斜体、行内代码和安全的 HTTP(S)/mailto 链接。按 <kbd>Ctrl</kbd>/<kbd>⌘</kbd> + <kbd>S</kbd> 可直接导出。</p>
    </section>
  );
}
