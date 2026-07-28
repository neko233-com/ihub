/**
 * A deliberately small Markdown reader for the built-in workbench.
 *
 * The workbench never hands document text to `innerHTML`. Instead it turns a
 * conservative Markdown subset into typed blocks which React renders as text
 * nodes. That keeps pasted HTML, event attributes, and javascript: URLs inert
 * while still covering the everyday notes and README workflow.
 */

export const MAX_MARKDOWN_DOCUMENT_CHARACTERS = 500_000;

export type MarkdownInline =
  | { kind: "text"; value: string }
  | { kind: "code"; value: string }
  | { kind: "strong"; children: MarkdownInline[] }
  | { kind: "emphasis"; children: MarkdownInline[] }
  | { kind: "link"; href: string; children: MarkdownInline[] };

export interface MarkdownHeading {
  level: number;
  id: string;
  text: string;
  inline: MarkdownInline[];
}

export type MarkdownBlock =
  | { kind: "heading"; heading: MarkdownHeading }
  | { kind: "paragraph"; inline: MarkdownInline[] }
  | { kind: "quote"; inline: MarkdownInline[] }
  | { kind: "code"; language: string | null; value: string }
  | { kind: "list"; ordered: boolean; start: number; items: MarkdownListItem[] }
  | { kind: "table"; headers: MarkdownInline[][]; rows: MarkdownInline[][][] }
  | { kind: "rule" };

export interface MarkdownListItem {
  inline: MarkdownInline[];
  checked?: boolean;
}

export interface MarkdownDocument {
  blocks: MarkdownBlock[];
  headings: MarkdownHeading[];
  truncated: boolean;
}

export interface MarkdownSummary {
  characters: number;
  words: number;
  readingMinutes: number;
}

function normalizedSource(source: string) {
  const normalized = source.replaceAll("\r\n", "\n").replaceAll("\r", "\n");
  return normalized.slice(0, MAX_MARKDOWN_DOCUMENT_CHARACTERS);
}

function plainText(inline: MarkdownInline[]): string {
  return inline.map((node) => {
    if (node.kind === "text" || node.kind === "code") {
      return node.value;
    }
    return plainText(node.children);
  }).join("");
}

function safeHref(candidate: string): string | null {
  const trimmed = candidate.trim();
  if (!trimmed || /[\u0000-\u001f]/.test(trimmed)) {
    return null;
  }

  // Relative anchors remain useful for local documentation. Absolute links are
  // intentionally limited to protocols that cannot execute script in a page.
  if (trimmed.startsWith("#") || trimmed.startsWith("/")) {
    return trimmed;
  }
  try {
    const parsed = new URL(trimmed);
    return ["https:", "http:", "mailto:"].includes(parsed.protocol) ? trimmed : null;
  } catch {
    return null;
  }
}

/** Parses inline emphasis without evaluating markup or accepting raw HTML. */
export function parseMarkdownInline(source: string): MarkdownInline[] {
  const nodes: MarkdownInline[] = [];
  let text = "";
  let index = 0;

  const appendText = (value: string) => {
    text += value;
  };
  const flushText = () => {
    if (text) {
      nodes.push({ kind: "text", value: text });
      text = "";
    }
  };

  while (index < source.length) {
    const current = source[index];
    if (current === "\\" && index + 1 < source.length) {
      appendText(source[index + 1]);
      index += 2;
      continue;
    }

    if (current === "`") {
      const close = source.indexOf("`", index + 1);
      if (close > index + 1) {
        flushText();
        nodes.push({ kind: "code", value: source.slice(index + 1, close) });
        index = close + 1;
        continue;
      }
    }

    if (current === "[" && source.indexOf("](", index + 1) > index) {
      const labelEnd = source.indexOf("](", index + 1);
      const hrefEnd = source.indexOf(")", labelEnd + 2);
      if (hrefEnd > labelEnd + 2) {
        const href = safeHref(source.slice(labelEnd + 2, hrefEnd));
        if (href) {
          flushText();
          nodes.push({
            kind: "link",
            href,
            children: parseMarkdownInline(source.slice(index + 1, labelEnd)),
          });
          index = hrefEnd + 1;
          continue;
        }
      }
    }

    const doubleMarker = source.slice(index, index + 2);
    if (doubleMarker === "**" || doubleMarker === "__") {
      const close = source.indexOf(doubleMarker, index + 2);
      if (close > index + 2) {
        flushText();
        nodes.push({
          kind: "strong",
          children: parseMarkdownInline(source.slice(index + 2, close)),
        });
        index = close + 2;
        continue;
      }
    }

    if (current === "*" || current === "_") {
      const close = source.indexOf(current, index + 1);
      if (close > index + 1) {
        flushText();
        nodes.push({
          kind: "emphasis",
          children: parseMarkdownInline(source.slice(index + 1, close)),
        });
        index = close + 1;
        continue;
      }
    }

    appendText(current);
    index += 1;
  }
  flushText();
  return nodes;
}

function slugifyHeading(value: string, occurrence: Map<string, number>) {
  const base = value
    .toLocaleLowerCase()
    .trim()
    .replace(/[^\p{L}\p{N}\s-]/gu, "")
    .replace(/[\s-]+/g, "-")
    .replace(/^-+|-+$/g, "") || "section";
  const next = (occurrence.get(base) ?? 0) + 1;
  occurrence.set(base, next);
  return next === 1 ? base : `${base}-${next}`;
}

function isFence(line: string) {
  return /^\s*```/.test(line);
}

function isSpecialBlockStart(line: string) {
  return isFence(line)
    || /^\s{0,3}#{1,6}\s+/.test(line)
    || /^\s*>\s?/.test(line)
    || /^\s*(?:[-+*])\s+/.test(line)
    || /^\s*\d+\.\s+/.test(line)
    || /^\s*(?:---+|\*\*\*+|___+)\s*$/.test(line);
}

function splitTableLine(line: string) {
  const trimmed = line.trim().replace(/^\|/, "").replace(/\|$/, "");
  return trimmed.split("|").map((cell) => cell.trim());
}

function isTableSeparator(line: string) {
  const cells = splitTableLine(line);
  return cells.length > 0 && cells.every((cell) => /^:?-{3,}:?$/.test(cell));
}

function taskItem(value: string): { checked?: boolean; text: string } {
  const matched = value.match(/^\[([ xX])\]\s+(.*)$/);
  if (!matched) {
    return { text: value };
  }
  return { checked: matched[1].toLocaleLowerCase() === "x", text: matched[2] };
}

export function parseMarkdown(source: string): MarkdownDocument {
  const input = normalizedSource(source);
  const lines = input.split("\n");
  const blocks: MarkdownBlock[] = [];
  const headings: MarkdownHeading[] = [];
  const occurrence = new Map<string, number>();
  let index = 0;

  while (index < lines.length) {
    const line = lines[index];
    if (!line.trim()) {
      index += 1;
      continue;
    }

    const headingMatch = line.match(/^\s{0,3}(#{1,6})\s+(.+?)\s*#*\s*$/);
    if (headingMatch) {
      const inline = parseMarkdownInline(headingMatch[2]);
      const text = plainText(inline).trim();
      const heading: MarkdownHeading = {
        level: headingMatch[1].length,
        id: slugifyHeading(text, occurrence),
        text,
        inline,
      };
      headings.push(heading);
      blocks.push({ kind: "heading", heading });
      index += 1;
      continue;
    }

    if (/^\s*(?:---+|\*\*\*+|___+)\s*$/.test(line)) {
      blocks.push({ kind: "rule" });
      index += 1;
      continue;
    }

    if (isFence(line)) {
      const language = line.replace(/^\s*```/, "").trim() || null;
      const codeLines: string[] = [];
      index += 1;
      while (index < lines.length && !isFence(lines[index])) {
        codeLines.push(lines[index]);
        index += 1;
      }
      if (index < lines.length) {
        index += 1;
      }
      blocks.push({ kind: "code", language, value: codeLines.join("\n") });
      continue;
    }

    if (/^\s*>\s?/.test(line)) {
      const quote: string[] = [];
      while (index < lines.length && /^\s*>\s?/.test(lines[index])) {
        quote.push(lines[index].replace(/^\s*>\s?/, ""));
        index += 1;
      }
      blocks.push({ kind: "quote", inline: parseMarkdownInline(quote.join(" ")) });
      continue;
    }

    const unordered = line.match(/^\s*(?:[-+*])\s+(.+)$/);
    const ordered = line.match(/^\s*(\d+)\.\s+(.+)$/);
    if (unordered || ordered) {
      const isOrdered = Boolean(ordered);
      const start = ordered ? Number.parseInt(ordered[1], 10) : 1;
      const items: MarkdownListItem[] = [];
      while (index < lines.length) {
        const match = isOrdered
          ? lines[index].match(/^\s*\d+\.\s+(.+)$/)
          : lines[index].match(/^\s*(?:[-+*])\s+(.+)$/);
        if (!match) {
          break;
        }
        const task = taskItem(match[1]);
        items.push({ checked: task.checked, inline: parseMarkdownInline(task.text) });
        index += 1;
      }
      blocks.push({ kind: "list", ordered: isOrdered, start, items });
      continue;
    }

    if (index + 1 < lines.length && line.includes("|") && isTableSeparator(lines[index + 1])) {
      const headerCells = splitTableLine(line);
      const columnCount = headerCells.length;
      const rows: MarkdownInline[][][] = [];
      index += 2;
      while (index < lines.length && lines[index].includes("|") && lines[index].trim()) {
        const cells = splitTableLine(lines[index]).slice(0, columnCount);
        while (cells.length < columnCount) {
          cells.push("");
        }
        rows.push(cells.map((cell) => parseMarkdownInline(cell)));
        index += 1;
      }
      blocks.push({
        kind: "table",
        headers: headerCells.map((cell) => parseMarkdownInline(cell)),
        rows,
      });
      continue;
    }

    const paragraph: string[] = [line.trim()];
    index += 1;
    while (index < lines.length && lines[index].trim() && !isSpecialBlockStart(lines[index])) {
      if (lines[index].includes("|") && index + 1 < lines.length && isTableSeparator(lines[index + 1])) {
        break;
      }
      paragraph.push(lines[index].trim());
      index += 1;
    }
    blocks.push({ kind: "paragraph", inline: parseMarkdownInline(paragraph.join(" ")) });
  }

  return {
    blocks,
    headings,
    truncated: source.length > MAX_MARKDOWN_DOCUMENT_CHARACTERS,
  };
}

export function summarizeMarkdown(source: string): MarkdownSummary {
  const characters = source.length;
  const words = source.trim() ? source.trim().split(/\s+/u).length : 0;
  return {
    characters,
    words,
    readingMinutes: Math.max(words ? 1 : 0, Math.ceil(words / 220)),
  };
}
