import { describe, expect, it } from "vitest";
import { parseMarkdown, parseMarkdownInline, summarizeMarkdown } from "./safe-markdown";

describe("safe Markdown parser", () => {
  it("builds a stable outline and common document blocks without HTML execution", () => {
    const document = parseMarkdown(`# Plan

## Plan

> Keep **this** local.

- [x] Done
- [ ] Next

| Key | Value |
| --- | --- |
| mode | offline |

\`\`\`ts
const text = "<img src=x onerror=alert(1)>";
\`\`\``);

    expect(document.headings.map((heading) => heading.id)).toEqual(["plan", "plan-2"]);
    expect(document.blocks.map((block) => block.kind)).toEqual([
      "heading",
      "heading",
      "quote",
      "list",
      "table",
      "code",
    ]);
    const list = document.blocks.find((block) => block.kind === "list");
    expect(list?.kind === "list" && list.items.map((item) => item.checked)).toEqual([true, false]);
    const code = document.blocks.find((block) => block.kind === "code");
    expect(code?.kind === "code" && code.value).toContain("onerror=alert(1)");
  });

  it("accepts only non-executable link protocols", () => {
    const inline = parseMarkdownInline("[safe](https://ihub.dev) [unsafe](javascript:alert(1)) [mail](mailto:test@example.com)");
    expect(inline.filter((node) => node.kind === "link").map((node) => node.kind === "link" ? node.href : "")).toEqual([
      "https://ihub.dev",
      "mailto:test@example.com",
    ]);
    expect(inline.some((node) => node.kind === "text" && node.value.includes("javascript:"))).toBe(true);
  });

  it("reports document metrics without sending or mutating source", () => {
    expect(summarizeMarkdown("one two three")).toEqual({ characters: 13, words: 3, readingMinutes: 1 });
    expect(summarizeMarkdown("   ")).toEqual({ characters: 3, words: 0, readingMinutes: 0 });
  });
});
