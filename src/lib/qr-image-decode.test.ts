import { describe, expect, it } from "vitest";
import { qrDecodeDimensions } from "./qr-image-decode";

describe("QR image decode bounds", () => {
  it("keeps ordinary images at their native dimensions", () => {
    expect(qrDecodeDimensions(1200, 800)).toEqual({ width: 1200, height: 800 });
  });

  it("downscales giant images proportionally before reading pixels", () => {
    const dimensions = qrDecodeDimensions(10_000, 5_000);
    expect(dimensions.width).toBeLessThanOrEqual(4096);
    expect(dimensions.width * dimensions.height).toBeLessThanOrEqual(16_000_000);
    expect(dimensions.width / dimensions.height).toBeCloseTo(2, 1);
  });

  it("rejects invalid image dimensions", () => {
    expect(() => qrDecodeDimensions(0, 100)).toThrow("图片尺寸无效");
  });
});
