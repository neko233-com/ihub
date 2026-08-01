export type RecordingSourcePreference = "monitor" | "window" | "browser";
export type RecordingQuality = "compact" | "balanced" | "smooth";

export interface ScreenRecordingProfile {
  frameRate: number;
  videoBitsPerSecond: number;
}

const profiles: Record<RecordingQuality, ScreenRecordingProfile> = {
  compact: { frameRate: 24, videoBitsPerSecond: 3_000_000 },
  balanced: { frameRate: 30, videoBitsPerSecond: 6_000_000 },
  smooth: { frameRate: 60, videoBitsPerSecond: 10_000_000 },
};

export function screenRecordingProfile(quality: RecordingQuality): ScreenRecordingProfile {
  return profiles[quality];
}

/**
 * The display surface is a preference only. Chromium/Windows keeps the final
 * source choice inside its trusted picker and may ignore this hint.
 */
export function createDisplayMediaOptions(
  source: RecordingSourcePreference,
  quality: RecordingQuality,
  includeSystemAudio: boolean,
): DisplayMediaStreamOptions {
  const profile = screenRecordingProfile(quality);
  return {
    audio: includeSystemAudio,
    video: {
      displaySurface: source,
      frameRate: {
        ideal: profile.frameRate,
        max: profile.frameRate,
      },
    },
  };
}

export function createMediaRecorderOptions(
  quality: RecordingQuality,
  mimeType?: string,
): MediaRecorderOptions {
  return {
    videoBitsPerSecond: screenRecordingProfile(quality).videoBitsPerSecond,
    ...(mimeType ? { mimeType } : {}),
  };
}
