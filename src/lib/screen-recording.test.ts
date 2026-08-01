import { describe, expect, it } from "vitest";
import {
  createDisplayMediaOptions,
  createMediaRecorderOptions,
  screenRecordingProfile,
} from "./screen-recording";

describe("screen recording capture preferences", () => {
  it("builds truthful system picker hints without exact constraints", () => {
    expect(createDisplayMediaOptions("window", "balanced", true)).toEqual({
      audio: true,
      video: {
        displaySurface: "window",
        frameRate: { ideal: 30, max: 30 },
      },
    });
  });

  it("maps the three bounded quality profiles to recorder bitrates", () => {
    expect(screenRecordingProfile("compact")).toEqual({ frameRate: 24, videoBitsPerSecond: 3_000_000 });
    expect(screenRecordingProfile("balanced")).toEqual({ frameRate: 30, videoBitsPerSecond: 6_000_000 });
    expect(screenRecordingProfile("smooth")).toEqual({ frameRate: 60, videoBitsPerSecond: 10_000_000 });
    expect(createMediaRecorderOptions("smooth", "video/webm;codecs=vp9,opus")).toEqual({
      mimeType: "video/webm;codecs=vp9,opus",
      videoBitsPerSecond: 10_000_000,
    });
  });
});
