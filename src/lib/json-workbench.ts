import { parse as parseYaml } from "yaml";

export type StructuredInputKind = "json" | "url-params" | "xml" | "yaml";
export type JsonWorkbenchValue = null | boolean | number | string | JsonWorkbenchValue[] | {
  [key: string]: JsonWorkbenchValue;
};

export interface ParsedStructuredInput {
  kind: StructuredInputKind;
  value: JsonWorkbenchValue;
}

export interface JsonPathResult {
  matches: JsonWorkbenchValue[];
  formatted: string;
}

const maxInputBytes = 2 * 1024 * 1024;
const maxQueryMatches = 512;

function assertBoundedInput(input: string): void {
  if (new TextEncoder().encode(input).byteLength > maxInputBytes) {
    throw new Error("输入最多支持 2 MiB。");
  }
}

function normalizeJsonValue(value: unknown): JsonWorkbenchValue {
  const encoded = JSON.stringify(value);
  if (encoded === undefined) {
    throw new Error("输入没有可转换的结构化内容。");
  }
  return JSON.parse(encoded) as JsonWorkbenchValue;
}

function scalarFromUrl(value: string): JsonWorkbenchValue {
  return value;
}

function parseUrlParams(input: string): JsonWorkbenchValue | null {
  let query = input.trim();
  try {
    const url = new URL(query);
    if (!url.search) {
      return null;
    }
    query = url.search.slice(1);
  } catch {
    if (query.startsWith("?")) {
      query = query.slice(1);
    }
    if (!query.includes("=") || (!query.includes("&") && /[\n{}\[\]<>]/.test(query))) {
      return null;
    }
  }

  const params = new URLSearchParams(query);
  if (![...params.keys()].length) {
    return null;
  }
  const result: Record<string, JsonWorkbenchValue> = Object.create(null) as Record<string, JsonWorkbenchValue>;
  params.forEach((value, key) => {
    const nextValue = scalarFromUrl(value);
    const current = result[key];
    if (current === undefined) {
      result[key] = nextValue;
    } else if (Array.isArray(current)) {
      current.push(nextValue);
    } else {
      result[key] = [current, nextValue];
    }
  });
  return result;
}

function xmlElementToValue(element: Element): JsonWorkbenchValue {
  const result: Record<string, JsonWorkbenchValue> = Object.create(null) as Record<string, JsonWorkbenchValue>;
  for (const attribute of [...element.attributes]) {
    result[`@${attribute.name}`] = attribute.value;
  }

  const children = [...element.children];
  for (const child of children) {
    const childValue = xmlElementToValue(child);
    const current = result[child.tagName];
    if (current === undefined) {
      result[child.tagName] = childValue;
    } else if (Array.isArray(current)) {
      current.push(childValue);
    } else {
      result[child.tagName] = [current, childValue];
    }
  }

  const text = [...element.childNodes]
    .filter((node) => node.nodeType === Node.TEXT_NODE || node.nodeType === Node.CDATA_SECTION_NODE)
    .map((node) => node.textContent ?? "")
    .join("")
    .trim();
  if (!children.length && !element.attributes.length) {
    return text;
  }
  if (text) {
    result["#text"] = text;
  }
  return result;
}

function parseXml(input: string): JsonWorkbenchValue | null {
  if (!input.trimStart().startsWith("<")) {
    return null;
  }
  if (typeof DOMParser === "undefined") {
    throw new Error("当前环境不支持 XML 解析。");
  }
  const document = new DOMParser().parseFromString(input, "application/xml");
  if (document.querySelector("parsererror")) {
    throw new Error("XML 语法无效。");
  }
  const root = document.documentElement;
  return { [root.tagName]: xmlElementToValue(root) };
}

export function parseStructuredInput(input: string): ParsedStructuredInput {
  assertBoundedInput(input);
  const trimmed = input.trim();
  if (!trimmed) {
    throw new Error("请先输入 JSON、URL Params、XML 或 YAML。");
  }

  try {
    return { kind: "json", value: normalizeJsonValue(JSON.parse(trimmed)) };
  } catch {
    // Continue through the explicitly supported local conversion formats.
  }

  const urlParams = parseUrlParams(trimmed);
  if (urlParams) {
    return { kind: "url-params", value: normalizeJsonValue(urlParams) };
  }

  const xml = parseXml(trimmed);
  if (xml) {
    return { kind: "xml", value: normalizeJsonValue(xml) };
  }

  try {
    return { kind: "yaml", value: normalizeJsonValue(parseYaml(trimmed)) };
  } catch (error) {
    throw new Error(error instanceof Error ? `无法解析输入：${error.message}` : "无法解析输入。");
  }
}

export function formatJsonValue(value: JsonWorkbenchValue): string {
  return JSON.stringify(value, null, 2);
}

export function minifyJsonValue(value: JsonWorkbenchValue): string {
  return JSON.stringify(value);
}

export function escapeJsonValue(value: JsonWorkbenchValue): string {
  return JSON.stringify(JSON.stringify(value));
}

function xmlName(name: string): string {
  const normalized = name.replace(/[^A-Za-z0-9_.-]/g, "_");
  return /^[A-Za-z_]/.test(normalized) ? normalized : `item_${normalized}`;
}

function escapeXml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&apos;");
}

function renderXmlNode(name: string, value: JsonWorkbenchValue, depth: number): string {
  const indentation = "  ".repeat(depth);
  const tag = xmlName(name);
  if (Array.isArray(value)) {
    return value.map((item) => renderXmlNode(tag, item, depth)).join("\n");
  }
  if (value !== null && typeof value === "object") {
    const attributes = Object.entries(value)
      .filter(([key, item]) => key.startsWith("@") && typeof item !== "object")
      .map(([key, item]) => ` ${xmlName(key.slice(1))}="${escapeXml(String(item))}"`)
      .join("");
    const text = value["#text"];
    const children = Object.entries(value).filter(([key]) => !key.startsWith("@") && key !== "#text");
    if (!children.length) {
      return `${indentation}<${tag}${attributes}>${text == null ? "" : escapeXml(String(text))}</${tag}>`;
    }
    const renderedChildren = children.map(([key, item]) => renderXmlNode(key, item, depth + 1)).join("\n");
    const renderedText = text == null ? "" : `${escapeXml(String(text))}\n`;
    return `${indentation}<${tag}${attributes}>\n${renderedText}${renderedChildren}\n${indentation}</${tag}>`;
  }
  return `${indentation}<${tag}>${value == null ? "" : escapeXml(String(value))}</${tag}>`;
}

export function jsonValueToXml(value: JsonWorkbenchValue): string {
  if (value !== null && !Array.isArray(value) && typeof value === "object") {
    const entries = Object.entries(value);
    if (entries.length === 1 && !entries[0]![0].startsWith("@")) {
      return `<?xml version="1.0" encoding="UTF-8"?>\n${renderXmlNode(entries[0]![0], entries[0]![1], 0)}`;
    }
  }
  return `<?xml version="1.0" encoding="UTF-8"?>\n${renderXmlNode("root", value, 0)}`;
}

function pascalCase(value: string): string {
  const words = value.split(/[^A-Za-z0-9]+/).filter(Boolean);
  const joined = words.map((word) => word.charAt(0).toUpperCase() + word.slice(1)).join("") || "Value";
  return /^[A-Za-z_$]/.test(joined) ? joined : `Value${joined}`;
}

function propertyName(value: string): string {
  return /^[A-Za-z_$][A-Za-z0-9_$]*$/.test(value) ? value : JSON.stringify(value);
}

function renderType(value: JsonWorkbenchValue, suggestedName: string, definitions: Map<string, string>): string {
  if (value === null) return "null";
  if (typeof value === "string") return "string";
  if (typeof value === "number") return "number";
  if (typeof value === "boolean") return "boolean";
  if (Array.isArray(value)) {
    if (!value.length) return "unknown[]";
    const types = [...new Set(value.map((item) => renderType(item, suggestedName, definitions)))];
    return types.length === 1 ? `${types[0]}[]` : `Array<${types.join(" | ")}>`;
  }

  let name = pascalCase(suggestedName);
  let suffix = 2;
  while (definitions.has(name)) {
    name = `${pascalCase(suggestedName)}${suffix}`;
    suffix += 1;
  }
  definitions.set(name, "");
  const fields = Object.entries(value).map(([key, item]) => {
    const type = renderType(item, `${name} ${key}`, definitions);
    return `  ${propertyName(key)}: ${type};`;
  });
  definitions.set(name, `export interface ${name} {\n${fields.join("\n")}\n}`);
  return name;
}

export function jsonValueToTypeScript(value: JsonWorkbenchValue): string {
  const definitions = new Map<string, string>();
  const rootType = renderType(value, "Root", definitions);
  const rendered = [...definitions.values()].filter(Boolean).reverse();
  if (rendered.length && rootType === "Root") {
    return rendered.join("\n\n");
  }
  return `${rendered.join("\n\n")}${rendered.length ? "\n\n" : ""}export type Root = ${rootType};`;
}

type JsonPathStep = { kind: "field"; value: string } | { kind: "index"; value: number } | { kind: "wildcard" };

function parseJsonPath(selector: string): JsonPathStep[] {
  const path = selector.trim();
  if (!path.startsWith("$")) {
    throw new Error("查询路径必须以 $ 开头。");
  }
  const steps: JsonPathStep[] = [];
  let index = 1;
  while (index < path.length) {
    if (path[index] === ".") {
      const match = path.slice(index + 1).match(/^[A-Za-z_$][A-Za-z0-9_$-]*/);
      if (!match) throw new Error("点号后需要字段名。");
      steps.push({ kind: "field", value: match[0] });
      index += match[0].length + 1;
      continue;
    }
    if (path[index] !== "[") throw new Error(`无法识别查询路径第 ${index + 1} 个字符。`);
    const closing = path.indexOf("]", index + 1);
    if (closing < 0) throw new Error("查询路径缺少 ]。");
    const content = path.slice(index + 1, closing).trim();
    if (content === "*") {
      steps.push({ kind: "wildcard" });
    } else if (/^\d+$/.test(content)) {
      steps.push({ kind: "index", value: Number(content) });
    } else {
      const quoted = content.match(/^(['"])(.*)\1$/s);
      if (!quoted) throw new Error("方括号仅支持索引、* 或引号字段名。");
      steps.push({ kind: "field", value: quoted[2]!.replace(/\\(['"\\])/g, "$1") });
    }
    index = closing + 1;
  }
  return steps;
}

export function queryJsonPath(value: JsonWorkbenchValue, selector: string): JsonPathResult {
  const steps = parseJsonPath(selector);
  let current = [value];
  for (const step of steps) {
    const next: JsonWorkbenchValue[] = [];
    for (const candidate of current) {
      if (step.kind === "field" && candidate !== null && !Array.isArray(candidate) && typeof candidate === "object") {
        if (Object.prototype.hasOwnProperty.call(candidate, step.value)) next.push(candidate[step.value]!);
      } else if (step.kind === "index" && Array.isArray(candidate) && candidate[step.value] !== undefined) {
        next.push(candidate[step.value]!);
      } else if (step.kind === "wildcard") {
        if (Array.isArray(candidate)) next.push(...candidate);
        else if (candidate !== null && typeof candidate === "object") next.push(...Object.values(candidate));
      }
      if (next.length > maxQueryMatches) throw new Error("查询结果超过 512 项，请缩小路径范围。");
    }
    current = next;
  }
  const result: JsonWorkbenchValue = current.length === 1 ? current[0]! : current;
  return { matches: current, formatted: formatJsonValue(result) };
}
