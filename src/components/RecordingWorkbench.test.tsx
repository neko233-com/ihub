import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { RecordingWorkbench, type RecordingPhase } from "./RecordingWorkbench";

function renderRecorder(phase: RecordingPhase, withResult = false): string {
  return renderToStaticMarkup(
    <RecordingWorkbench
      bytes={phase === "idle" ? 0 : 12_582_912}
      elapsedMs={phase === "idle" ? 0 : 65_000}
      includeSystemAudio
      maximumBytes={512 * 1024 * 1024}
      maximumDurationMs={30 * 60 * 1_000}
      onClose={() => undefined}
      onIncludeSystemAudioChange={() => undefined}
      onPause={() => undefined}
      onQualityChange={() => undefined}
      onResume={() => undefined}
      onSourcePreferenceChange={() => undefined}
      onStart={() => undefined}
      onStop={() => undefined}
      phase={phase}
      quality="balanced"
      result={withResult ? {
        durationMs: 65_000,
        mimeType: "video/webm",
        name: "capture.webm",
        size: 12_582_912,
        url: "blob:recording-preview",
      } : null}
      sourceName={phase === "idle" ? null : "Display 1"}
      sourcePreference="monitor"
    />,
  );
}

describe("RecordingWorkbench", () => {
  it("exposes source preferences and bounded real capture settings", () => {
    const markup = renderRecorder("idle");
    expect(markup).toContain('aria-label="录制来源偏好"');
    expect(markup).toContain("整个屏幕");
    expect(markup).toContain("应用窗口");
    expect(markup).toContain("浏览器标签");
    expect(markup).toContain("打开系统选择器并录制");
    expect(markup).toContain("30 分钟 · 512 MiB");
    expect(markup).toContain("这里不会记录键盘输入");
  });

  it("shows pause and stop controls only during active recording", () => {
    const markup = renderRecorder("recording");
    expect(markup).toContain("正在录制");
    expect(markup).toContain("暂停");
    expect(markup).toContain("停止并保存");
    expect(markup).toContain("Display 1");
  });

  it("keeps the completed WebM available for local preview and redownload", () => {
    const markup = renderRecorder("idle", true);
    expect(markup).toContain('aria-label="最近一次录屏预览"');
    expect(markup).toContain('src="blob:recording-preview"');
    expect(markup).toContain("最近录制已保存");
    expect(markup).toContain("再次下载");
  });
});
