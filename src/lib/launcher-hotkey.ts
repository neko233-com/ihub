/**
 * The narrow KeyboardEvent surface needed while recording a launcher hotkey.
 * `code` is used instead of `key` so the saved accelerator is independent of
 * the active keyboard layout and matches the physical-code names parsed by the
 * native global-shortcut crate.
 */
export interface LauncherHotkeyKeyboardEvent {
  code: string;
  altKey?: boolean;
  ctrlKey?: boolean;
  metaKey?: boolean;
  shiftKey?: boolean;
}

export type LauncherHotkeyRejectionReason =
  | "modifier-required"
  | "modifier-only"
  | "reserved-key"
  | "reserved-shortcut"
  | "unsupported-key";

export type LauncherHotkeyNormalizationResult =
  | {
    ok: true;
    /** Canonical modifier order consumed by the native Rust store. */
    accelerator: string;
  }
  | {
    ok: false;
    reason: LauncherHotkeyRejectionReason;
  };

const modifierCodes = new Set([
  "AltLeft",
  "AltRight",
  "ControlLeft",
  "ControlRight",
  "MetaLeft",
  "MetaRight",
  "ShiftLeft",
  "ShiftRight",
]);

const reservedCodes = new Set([
  "Tab",
  "Escape",
  "Enter",
  "NumpadEnter",
  "Delete",
  "ArrowUp",
  "ArrowDown",
  "ArrowLeft",
  "ArrowRight",
]);

/** Punctuation keys with stable physical codes supported on Windows and macOS. */
const safePunctuationCodes = new Set([
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
]);

function canonicalKey(code: string): string | null {
  if (
    code === "Space"
    || /^Key[A-Z]$/.test(code)
    || /^Digit[0-9]$/.test(code)
    || /^F(?:[1-9]|1[0-2])$/.test(code)
    || safePunctuationCodes.has(code)
  ) {
    return code;
  }
  return null;
}

/**
 * Converts one deliberate key press into the exact accelerator spelling stored
 * by the native host. Ctrl and Meta both mean the cross-platform CmdOrCtrl
 * intent; pressing both still produces only one canonical modifier.
 */
export function normalizeLauncherHotkey(
  event: LauncherHotkeyKeyboardEvent,
): LauncherHotkeyNormalizationResult {
  const code = event.code.trim();
  if (modifierCodes.has(code)) {
    return { ok: false, reason: "modifier-only" };
  }
  if (reservedCodes.has(code)) {
    return { ok: false, reason: "reserved-key" };
  }

  const key = canonicalKey(code);
  if (!key) {
    return { ok: false, reason: "unsupported-key" };
  }

  const hasCmdOrCtrl = Boolean(event.ctrlKey || event.metaKey);
  const hasAlt = Boolean(event.altKey);
  if (!hasCmdOrCtrl && !hasAlt) {
    return { ok: false, reason: "modifier-required" };
  }
  if (hasAlt && key === "F4") {
    return { ok: false, reason: "reserved-shortcut" };
  }

  const tokens: string[] = [];
  if (hasCmdOrCtrl) {
    tokens.push("CmdOrCtrl");
  }
  if (hasAlt) {
    tokens.push("Alt");
  }
  if (event.shiftKey) {
    tokens.push("Shift");
  }
  tokens.push(key);
  return { ok: true, accelerator: tokens.join("+") };
}

function isMacPlatform(platform: string) {
  const normalized = platform.trim().toLocaleLowerCase();
  return normalized === "darwin"
    || normalized === "mac"
    || normalized === "macos"
    || normalized.startsWith("macos-")
    || normalized.startsWith("darwin-");
}

const punctuationLabels: Readonly<Record<string, string>> = {
  BACKQUOTE: "`",
  BACKSLASH: "\\",
  BRACKETLEFT: "[",
  BRACKETRIGHT: "]",
  COMMA: ",",
  EQUAL: "=",
  MINUS: "-",
  PERIOD: ".",
  QUOTE: "'",
  SEMICOLON: ";",
  SLASH: "/",
};

function formatAcceleratorToken(token: string, mac: boolean) {
  const upper = token.toLocaleUpperCase();
  switch (upper) {
    case "COMMANDORCONTROL":
    case "COMMANDORCTRL":
    case "CMDORCONTROL":
    case "CMDORCTRL":
      return mac ? "Command" : "Ctrl";
    case "ALT":
    case "OPTION":
      return mac ? "Option" : "Alt";
    case "CONTROL":
    case "CTRL":
      return mac ? "Control" : "Ctrl";
    case "COMMAND":
    case "CMD":
      return "Command";
    case "META":
    case "SUPER":
      return mac ? "Command" : "Super";
    case "SHIFT":
      return "Shift";
    case "SPACE":
      return "Space";
    default:
      if (/^KEY[A-Z]$/.test(upper)) {
        return upper.slice(-1);
      }
      if (/^DIGIT[0-9]$/.test(upper)) {
        return upper.slice(-1);
      }
      if (/^F(?:[1-9]|1[0-2])$/.test(upper)) {
        return upper;
      }
      return punctuationLabels[upper] ?? token;
  }
}

/**
 * Formats both current canonical accelerators and legacy native-shell strings.
 * Unknown tokens are preserved so the UI never hides what the host reported.
 */
export function formatLauncherHotkey(
  accelerator: string,
  platform = "windows",
): string {
  const trimmed = accelerator.trim();
  if (!trimmed) {
    return "";
  }
  const tokens = trimmed.split("+").map((token) => token.trim());
  if (tokens.some((token) => !token)) {
    return trimmed;
  }
  const mac = isMacPlatform(platform);
  return tokens
    .map((token) => formatAcceleratorToken(token, mac))
    .join(" + ");
}
