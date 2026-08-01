import { describe, expect, it } from "vitest";
import { boundedOcrImageSize } from "./ocr-image";

describe("boundedOcrImageSize", () => {
  it("preserves small image dimensions", () => {
    expect(boundedOcrImageSize(1200, 800, 2600)).toEqual({ width: 1200, height: 800 });
  });

  it("scales oversized captures without changing aspect ratio", () => {
    expect(boundedOcrImageSize(3840, 2160, 2600)).toEqual({ width: 2600, height: 1463 });
  });

  it("rejects invalid dimensions", () => {
    expect(() => boundedOcrImageSize(0, 800, 2600)).toThrow("OCR 图片尺寸无效");
  });
});
