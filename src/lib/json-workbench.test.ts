import { describe, expect, it } from "vitest";
import {
  escapeJsonValue,
  formatJsonValue,
  jsonValueToTypeScript,
  jsonValueToXml,
  minifyJsonValue,
  parseStructuredInput,
  queryJsonPath,
  type JsonWorkbenchValue,
} from "./json-workbench";

describe("JSON workbench input conversion", () => {
  it("keeps JSON canonical and converts URL params with repeated keys", () => {
    expect(parseStructuredInput('{"name":"iHub"}')).toEqual({
      kind: "json",
      value: { name: "iHub" },
    });
    expect(parseStructuredInput("name=iHub&tag=fast&tag=local")).toEqual({
      kind: "url-params",
      value: { name: "iHub", tag: ["fast", "local"] },
    });
  });

  it("converts YAML locally without changing scalar types", () => {
    expect(parseStructuredInput("name: iHub\nfeatures:\n  - search\n  - plugins\nenabled: true")).toEqual({
      kind: "yaml",
      value: { name: "iHub", features: ["search", "plugins"], enabled: true },
    });
  });

  it("rejects empty or oversized input before parsing", () => {
    expect(() => parseStructuredInput("  ")).toThrow("请先输入");
    expect(() => parseStructuredInput("x".repeat(2 * 1024 * 1024 + 1))).toThrow("2 MiB");
  });
});

describe("JSON workbench transforms", () => {
  const value: JsonWorkbenchValue = { name: "iHub", flags: [true, false], meta: { count: 2 } };

  it("formats, minifies and escapes JSON deterministically", () => {
    expect(formatJsonValue(value)).toContain('\n  "name": "iHub"');
    expect(minifyJsonValue(value)).toBe('{"name":"iHub","flags":[true,false],"meta":{"count":2}}');
    expect(JSON.parse(escapeJsonValue(value))).toBe(minifyJsonValue(value));
  });

  it("renders XML and nested TypeScript interfaces", () => {
    expect(jsonValueToXml({ project: value })).toContain("<project>");
    const types = jsonValueToTypeScript(value);
    expect(types).toContain("export interface Root");
    expect(types).toContain("meta: RootMeta;");
    expect(types).toContain("flags: boolean[];");
  });
});

describe("bounded JSONPath querying", () => {
  const value = {
    items: [{ id: "first" }, { id: "second" }],
    "display-name": "iHub",
  };

  it("supports fields, quoted keys, indexes and wildcards without eval", () => {
    expect(queryJsonPath(value, "$.items[1].id").formatted).toBe('"second"');
    expect(queryJsonPath(value, "$['display-name']").formatted).toBe('"iHub"');
    expect(queryJsonPath(value, "$.items[*].id")).toEqual({
      matches: ["first", "second"],
      formatted: '[\n  "first",\n  "second"\n]',
    });
  });

  it("rejects executable or recursive selector syntax", () => {
    for (const selector of ["items", "$..items", "$.items[?(@.id)]", "$.items[-1]"]) {
      expect(() => queryJsonPath(value, selector)).toThrow();
    }
  });
});
