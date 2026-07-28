import { describe, expect, it } from "vitest";
import { describeLauncherHotkey } from "./launcher-hotkey-status";

describe("describeLauncherHotkey", () => {
  it("shows the native primary binding with a platform-specific label", () => {
    const presentation = describeLauncherHotkey({
      registration: "primary",
      accelerator: "Alt+Space",
      trayShowAvailable: true,
    });

    expect(presentation.shortcutLabel).toBe("Alt + Space");
    expect(presentation.footerText).toContain("呼出");
    expect(presentation.ariaLabel).toContain("已注册");

    const macPresentation = describeLauncherHotkey({
      registration: "primary",
      accelerator: "Alt+Space",
      trayShowAvailable: true,
    }, true, "macos");
    expect(macPresentation.shortcutLabel).toBe("Option + Space");
  });

  it("honestly names the occupied preference and the registered fallback", () => {
    const presentation = describeLauncherHotkey({
      registration: "fallback",
      accelerator: "CmdOrCtrl+Shift+KeyK",
      preferredAccelerator: "CmdOrCtrl+Space",
      trayShowAvailable: true,
    });

    expect(presentation.shortcutLabel).toBe("Ctrl + Shift + K");
    expect(presentation.settingsDescription).toContain("首选 Ctrl + Space 已被占用");
    expect(presentation.ariaLabel).toContain("备用快捷键");
  });

  it("keeps the legacy fallback shell compatible when it has no preferred field", () => {
    const presentation = describeLauncherHotkey({
      registration: "fallback",
      accelerator: "Alt+Shift+Space",
      trayShowAvailable: true,
    }, true, "macos");

    expect(presentation.shortcutLabel).toBe("Option + Shift + Space");
    expect(presentation.settingsDescription).toContain("首选 Option + Space 已被占用");
  });

  it("describes a successfully configured arbitrary accelerator", () => {
    const presentation = describeLauncherHotkey({
      registration: "configured",
      accelerator: "CmdOrCtrl+Alt+Slash",
      trayShowAvailable: true,
    }, true, "macos");

    expect(presentation.shortcutLabel).toBe("Command + Option + /");
    expect(presentation.footerText).toBe("呼出：Command + Option + /");
    expect(presentation.settingsDescription).toContain("自定义快捷键");
  });

  it("keeps the tray Show recovery path visible when both registrations fail", () => {
    const presentation = describeLauncherHotkey({
      registration: "unavailable",
      trayShowAvailable: true,
    });

    expect(presentation.shortcutLabel).toBeUndefined();
    expect(presentation.footerText).toContain("托盘 Show");
    expect(presentation.settingsDescription).toContain("Show iHub");
    expect(presentation.ariaLabel).toContain("Show iHub");
  });

  it("does not guess the registration result for an older native shell", () => {
    const presentation = describeLauncherHotkey(undefined);

    expect(presentation.footerText).toBeUndefined();
    expect(presentation.settingsDescription).toContain("尚未返回");
  });

  it("does not imply a system shortcut exists in browser preview", () => {
    const presentation = describeLauncherHotkey(undefined, false);

    expect(presentation.footerText).toBeUndefined();
    expect(presentation.settingsDescription).toContain("浏览器预览");
  });

  it("does not claim success when a registered status omits its accelerator", () => {
    const presentation = describeLauncherHotkey({
      registration: "configured",
      trayShowAvailable: true,
    });

    expect(presentation.shortcutLabel).toBeUndefined();
    expect(presentation.footerText).toContain("状态异常");
    expect(presentation.settingsDescription).toContain("没有返回实际按键");
  });
});
