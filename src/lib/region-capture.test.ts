import { describe, expect, it } from "vitest";
import {
  captureRegionStyle,
  isUsableCaptureRegion,
  pointInCaptureSource,
  regionFromDrag,
  validateRegionCaptureSize,
} from "./region-capture";

describe("region capture geometry", () => {
  it("normalizes reverse drags and clamps both endpoints to the source", () => {
    expect(regionFromDrag(
      { x: 830.4, y: 460.1 },
      { x: -90, y: 20.2 },
      { width: 800, height: 500 },
    )).toEqual({
      x: 0,
      y: 20,
      width: 800,
      height: 441,
    });
  });

  it("maps rendered CSS pixels to source pixels without depending on DPI", () => {
    expect(pointInCaptureSource(
      { x: 250, y: 175 },
      { x: 50, y: 40, width: 400, height: 270 },
      { width: 960, height: 540 },
    )).toEqual({ x: 480, y: 270 });
  });

  it("requires a real region and expresses its overlay as source percentages", () => {
    expect(isUsableCaptureRegion({ x: 1, y: 1, width: 1, height: 12 })).toBe(false);
    const region = { x: 96, y: 54, width: 480, height: 270 };
    expect(isUsableCaptureRegion(region)).toBe(true);
    expect(captureRegionStyle(region, { width: 960, height: 540 })).toEqual({
      left: "10%",
      top: "10%",
      width: "50%",
      height: "50%",
    });
  });

  it("rejects empty and unbounded source frames before canvas allocation", () => {
    expect(() => validateRegionCaptureSize({ width: 0, height: 20 })).toThrow();
    expect(() => validateRegionCaptureSize({ width: 8_193, height: 1 })).toThrow();
    expect(() => validateRegionCaptureSize({ width: 8_000, height: 4_000 })).toThrow();
  });
});
