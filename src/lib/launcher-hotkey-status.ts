import type { LauncherHotkeyStatus } from "./types";
import { formatLauncherHotkey } from "./launcher-hotkey";

export interface LauncherHotkeyPresentation {
  /** Compact factual text for the launcher's existing footer. */
  footerText?: string;
  /** Full recovery-aware explanation for Preferences. */
  settingsDescription: string;
  /** The resolved key combination, when registration succeeded. */
  shortcutLabel?: string;
  /** A complete status announcement for assistive technology. */
  ariaLabel: string;
}

function registeredShortcut(
  status: LauncherHotkeyStatus,
  platform: string,
): string | null {
  const accelerator = status.accelerator?.trim();
  return accelerator ? formatLauncherHotkey(accelerator, platform) : null;
}

function missingAcceleratorPresentation(): LauncherHotkeyPresentation {
  return {
    footerText: "快捷键状态异常 · 托盘 Show 可打开",
    settingsDescription: "原生壳报告快捷键已注册，但没有返回实际按键；仍可从系统托盘菜单选择“Show iHub”打开 iHub。",
    ariaLabel: "iHub 原生壳没有返回已注册快捷键的实际按键。仍可从系统托盘菜单选择 Show iHub 打开 iHub。",
  };
}

/**
 * Converts the small, host-owned registration result into concise UI copy.
 * Registration itself remains in Rust; the renderer only describes the
 * resolved default, configured, fallback, or unavailable state.
 */
export function describeLauncherHotkey(
  status: LauncherHotkeyStatus | null | undefined,
  isDesktop = true,
  platform = "windows",
): LauncherHotkeyPresentation {
  if (!isDesktop) {
    return {
      settingsDescription: "浏览器预览不会注册系统级启动快捷键。",
      ariaLabel: "浏览器预览不会注册 iHub 系统级启动快捷键。",
    };
  }

  switch (status?.registration) {
    case "primary": {
      const shortcut = registeredShortcut(status, platform);
      if (!shortcut) {
        return missingAcceleratorPresentation();
      }
      return {
        footerText: `呼出：${shortcut}`,
        settingsDescription: `已注册 ${shortcut}，可在任意位置呼出 iHub。`,
        shortcutLabel: shortcut,
        ariaLabel: `iHub 启动快捷键已注册：${shortcut}。`,
      };
    }
    case "configured": {
      const shortcut = registeredShortcut(status, platform);
      if (!shortcut) {
        return missingAcceleratorPresentation();
      }
      return {
        footerText: `呼出：${shortcut}`,
        settingsDescription: `已注册自定义快捷键 ${shortcut}，可在任意位置呼出 iHub。`,
        shortcutLabel: shortcut,
        ariaLabel: `iHub 自定义启动快捷键已注册：${shortcut}。`,
      };
    }
    case "fallback": {
      const shortcut = registeredShortcut(status, platform);
      if (!shortcut) {
        return missingAcceleratorPresentation();
      }
      const preferred = formatLauncherHotkey(
        status.preferredAccelerator?.trim() || "Alt+Space",
        platform,
      );
      return {
        footerText: `备用呼出：${shortcut}`,
        settingsDescription: `首选 ${preferred} 已被占用；已改用 ${shortcut} 呼出 iHub。`,
        shortcutLabel: shortcut,
        ariaLabel: `iHub 首选启动快捷键 ${preferred} 不可用，已注册备用快捷键：${shortcut}。`,
      };
    }
    case "unavailable":
      return {
        footerText: "快捷键不可用 · 托盘 Show 可打开",
        settingsDescription: "未能注册启动快捷键；仍可从系统托盘菜单选择“Show iHub”打开 iHub。",
        ariaLabel: "iHub 启动快捷键不可用。仍可从系统托盘菜单选择 Show iHub 打开 iHub。",
      };
    default:
      // Old local shells do not expose the new health field. Do not claim a
      // shortcut is registered or unavailable until a current native host
      // supplies an explicit answer.
      return {
        settingsDescription: "当前原生壳尚未返回启动快捷键状态；仍可从系统托盘菜单选择“Show iHub”打开 iHub。",
        ariaLabel: "当前原生壳尚未返回 iHub 启动快捷键状态。仍可从系统托盘菜单选择 Show iHub 打开 iHub。",
      };
  }
}
