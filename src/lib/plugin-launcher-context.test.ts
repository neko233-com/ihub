import { describe, expect, it } from "vitest";
import {
  eligibleLauncherContextCommands,
  previewLauncherContextHandoff,
  type LauncherContextHandoff,
} from "./plugin-launcher-context";
import type { PluginInfo } from "./types";

const textHandoff: LauncherContextHandoff = {
  id: "launcher-context-test",
  kind: "text",
  suggestedUse: "翻译",
  text: "A deliberately explicit launcher text selection.",
};

function plugin(overrides: Partial<PluginInfo>): PluginInfo {
  return {
    id: "ihub-plugin-test",
    name: "Test plugin",
    version: "1.0.0",
    frontendEntry: "dist/index.html",
    enabled: true,
    commands: [{ id: "process", name: "Process", execution: "frontend" }],
    ...overrides,
  };
}

describe("explicit plugin launcher-context eligibility", () => {
  it("shows only enabled frontend commands with the exact declared category", () => {
    const candidates = eligibleLauncherContextCommands([
      plugin({ id: "text", launcherContext: { text: true } }),
      plugin({
        id: "native-only",
        launcherContext: { text: true },
        commands: [{ id: "worker", name: "Worker", execution: "native" }],
      }),
      plugin({ id: "files-only", launcherContext: { files: true } }),
      plugin({ id: "disabled", launcherContext: { text: true }, enabled: false }),
      plugin({ id: "missing-declaration" }),
    ], textHandoff);

    expect(candidates.map(({ plugin: candidate, command }) => `${candidate.id}/${command.id}`)).toEqual([
      "text/process",
    ]);
  });

  it("keeps source text out of the confirmation preview", () => {
    const preview = previewLauncherContextHandoff(textHandoff);

    expect(preview.categories).toEqual(["text"]);
    expect(preview.detail).toContain("字符");
    expect(preview.detail).not.toContain(textHandoff.text);
  });

  it("states that metadata handoffs still require system-picker reselection for bytes", () => {
    const filesPreview = previewLauncherContextHandoff({
      id: "files",
      kind: "files",
      suggestedUse: "OCR",
      files: [{ path: "D:\\Pictures\\capture.png", name: "capture.png", kind: "file" }],
    });
    const imagePreview = previewLauncherContextHandoff({
      id: "image",
      kind: "image",
      suggestedUse: "Image Tools",
      image: {
        blob: new Blob(["not-real-pixels"], { type: "image/png" }),
        name: "capture.png",
        type: "image/png",
      },
    });

    expect(filesPreview.detail).toContain("系统选择器中重新选择");
    expect(filesPreview.detail).toContain("不会收到路径");
    expect(imagePreview.detail).toContain("系统选择器中重新选择");
    expect(imagePreview.detail).toContain("不会收到像素");
  });
});
