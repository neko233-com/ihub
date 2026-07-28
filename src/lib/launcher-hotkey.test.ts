import { describe, expect, it } from "vitest";
import {
  formatLauncherHotkey,
  normalizeLauncherHotkey,
} from "./launcher-hotkey";

describe("normalizeLauncherHotkey", () => {
  it("emits the Rust store's canonical modifier spelling and order", () => {
    expect(normalizeLauncherHotkey({
      code: "KeyK",
      ctrlKey: true,
      metaKey: true,
      altKey: true,
      shiftKey: true,
    })).toEqual({
      ok: true,
      accelerator: "CmdOrCtrl+Alt+Shift+KeyK",
    });

    expect(normalizeLauncherHotkey({
      code: "Space",
      metaKey: true,
    })).toEqual({
      ok: true,
      accelerator: "CmdOrCtrl+Space",
    });
  });

  it.each([
    "Space",
    "KeyA",
    "KeyZ",
    "Digit0",
    "Digit9",
    "F1",
    "F12",
    "Backquote",
    "Minus",
    "Equal",
    "BracketLeft",
    "BracketRight",
    "Backslash",
    "Semicolon",
    "Quote",
    "Comma",
    "Period",
    "Slash",
  ])("accepts the cross-platform physical key %s", (code) => {
    expect(normalizeLauncherHotkey({ code, ctrlKey: true })).toMatchObject({
      ok: true,
    });
  });

  it("allows Alt as the primary modifier and Shift only as a supplement", () => {
    expect(normalizeLauncherHotkey({
      code: "Slash",
      altKey: true,
      shiftKey: true,
    })).toEqual({
      ok: true,
      accelerator: "Alt+Shift+Slash",
    });
    expect(normalizeLauncherHotkey({
      code: "KeyK",
      shiftKey: true,
    })).toEqual({
      ok: false,
      reason: "modifier-required",
    });
  });

  it.each([
    "Tab",
    "Escape",
    "Enter",
    "NumpadEnter",
    "Delete",
    "ArrowUp",
    "ArrowDown",
    "ArrowLeft",
    "ArrowRight",
  ])("rejects the reserved key %s", (code) => {
    expect(normalizeLauncherHotkey({ code, ctrlKey: true })).toEqual({
      ok: false,
      reason: "reserved-key",
    });
  });

  it("rejects modifier-only presses and unsupported keys", () => {
    expect(normalizeLauncherHotkey({
      code: "ControlLeft",
      ctrlKey: true,
    })).toEqual({
      ok: false,
      reason: "modifier-only",
    });
    expect(normalizeLauncherHotkey({
      code: "Numpad1",
      ctrlKey: true,
    })).toEqual({
      ok: false,
      reason: "unsupported-key",
    });
    expect(normalizeLauncherHotkey({
      code: "F13",
      altKey: true,
    })).toEqual({
      ok: false,
      reason: "unsupported-key",
    });
  });

  it("rejects every Alt+F4 variant but permits CmdOrCtrl+F4", () => {
    expect(normalizeLauncherHotkey({
      code: "F4",
      altKey: true,
      ctrlKey: true,
      shiftKey: true,
    })).toEqual({
      ok: false,
      reason: "reserved-shortcut",
    });
    expect(normalizeLauncherHotkey({
      code: "F4",
      ctrlKey: true,
    })).toEqual({
      ok: true,
      accelerator: "CmdOrCtrl+F4",
    });
  });
});

describe("formatLauncherHotkey", () => {
  it("uses platform-native names for cross-platform modifiers", () => {
    const accelerator = "CmdOrCtrl+Alt+Shift+KeyK";
    expect(formatLauncherHotkey(accelerator, "windows")).toBe("Ctrl + Alt + Shift + K");
    expect(formatLauncherHotkey(accelerator, "linux")).toBe("Ctrl + Alt + Shift + K");
    expect(formatLauncherHotkey(accelerator, "macos")).toBe("Command + Option + Shift + K");
    expect(formatLauncherHotkey(accelerator, "darwin-aarch64")).toBe("Command + Option + Shift + K");
  });

  it("formats physical digits, punctuation, and legacy native-shell aliases", () => {
    expect(formatLauncherHotkey("CmdOrCtrl+Digit7", "windows")).toBe("Ctrl + 7");
    expect(formatLauncherHotkey("Alt+Slash", "macos")).toBe("Option + /");
    expect(formatLauncherHotkey("CommandOrControl+Shift+Space", "macos")).toBe(
      "Command + Shift + Space",
    );
  });

  it("preserves an unknown host key instead of hiding it", () => {
    expect(formatLauncherHotkey("CmdOrCtrl+MediaPlayPause", "windows")).toBe(
      "Ctrl + MediaPlayPause",
    );
  });
});
