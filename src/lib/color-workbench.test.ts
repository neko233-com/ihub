import { describe, expect, it } from "vitest";
import {
  colorFormats,
  colorHarmonies,
  colorHexToRgb,
  colorHslToHex,
  colorRgbToHsl,
  normalizeColorHex,
  readableTextColor,
} from "./color-workbench";

describe("color workbench", () => {
  it("round-trips representative saturated colors", () => {
    for (const hex of ["#0A84FF", "#BF5AF2", "#30D158", "#FF375F"]) {
      expect(colorHslToHex(colorRgbToHsl(colorHexToRgb(hex)))).toBe(hex);
    }
  });

  it("projects the copy formats used by the workbench", () => {
    const formats = Object.fromEntries(colorFormats("#0A84FF", 0.5).map((entry) => [entry.label, entry.value]));
    expect(formats.HEX).toBe("#0A84FF");
    expect(formats.RGB).toBe("10, 132, 255");
    expect(formats.CSS).toBe("rgb(10 132 255 / 0.50)");
    expect(formats.CMYK).toContain("96%");
    expect(formats.OKLCH).toMatch(/% \d\.\d{4} \d+\.\d/);
  });

  it("generates stable harmony groups and accessible foregrounds", () => {
    expect(colorHarmonies("#FF0000")).toEqual([
      { label: "互补色", colors: ["#00FFFF"] },
      { label: "类似色", colors: ["#FF0080", "#FF8000"] },
      { label: "分裂互补", colors: ["#00FF80", "#0080FF"] },
      { label: "三角色", colors: ["#00FF00", "#0000FF"] },
    ]);
    expect(readableTextColor("#000000")).toBe("#FFFFFF");
    expect(readableTextColor("#FFFFFF")).toBe("#000000");
  });

  it("rejects ambiguous color input", () => {
    expect(() => normalizeColorHex("#fff")).toThrow();
    expect(() => normalizeColorHex("0A84FF")).toThrow();
  });
});
