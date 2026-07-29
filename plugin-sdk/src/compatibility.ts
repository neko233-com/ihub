import type {
  Disposable,
  IHubUToolsCompatibilityApi,
  PluginContext,
  PluginSubInputChangeHandler,
} from "./types.js";

const MAX_SUB_INPUT_PLACEHOLDER_LENGTH = 160;
const MAX_SUB_INPUT_VALUE_LENGTH = 4_096;

const noopDisposable: Disposable = {
  dispose() {
    // Nothing was installed.
  },
};

function platformText(): string {
  if (typeof navigator === "undefined") {
    return "";
  }
  return `${navigator.platform ?? ""} ${navigator.userAgent ?? ""}`.toLowerCase();
}

function validSubInputArguments(
  onChange: PluginSubInputChangeHandler,
  placeholder: string,
  focus: boolean,
): boolean {
  return (
    typeof onChange === "function"
    && typeof placeholder === "string"
    && placeholder.length <= MAX_SUB_INPUT_PLACEHOLDER_LENGTH
    && typeof focus === "boolean"
  );
}

/**
 * Installs only the small uTools/Rubick projection that iHub can implement
 * through its existing capability-checked SDK. Existing globals are never
 * replaced, and disposal restores the page's original property descriptors.
 */
export function installUToolsCompatibility(
  context: PluginContext,
  onError: (error: unknown) => void,
): Disposable {
  if (
    typeof window === "undefined"
    || window.utools !== undefined
    || window.rubick !== undefined
  ) {
    return noopDisposable;
  }

  const schedule = (operation: Promise<unknown>): true => {
    void operation.catch(onError);
    return true;
  };

  const api: IHubUToolsCompatibilityApi = Object.freeze({
    setSubInput(
      onChange: PluginSubInputChangeHandler,
      placeholder = "",
      isFocus = true,
    ): boolean {
      if (!validSubInputArguments(onChange, placeholder, isFocus)) {
        return false;
      }
      return schedule(context.subInput.set(onChange, placeholder, isFocus));
    },
    removeSubInput(): boolean {
      return schedule(context.subInput.remove());
    },
    setSubInputValue(value: string): boolean {
      if (typeof value !== "string" || value.length > MAX_SUB_INPUT_VALUE_LENGTH) {
        return false;
      }
      return schedule(context.subInput.setValue(value));
    },
    copyText(value: string): boolean {
      if (typeof value !== "string") {
        return false;
      }
      return schedule(context.clipboard.writeText(value));
    },
    showNotification(body: string): void {
      if (typeof body !== "string") {
        return;
      }
      schedule(context.notifications.show({
        title: context.pluginId,
        body,
      }));
    },
    shellOpenExternal(url: string): void {
      if (typeof url !== "string") {
        return;
      }
      schedule(context.shell.openExternal(url));
    },
    shellOpenPath(path: string): void {
      if (typeof path !== "string") {
        return;
      }
      schedule(context.shell.openPath(path));
    },
    screenColorPick(callback: (color: { hex: string; rgb: string }) => void): void {
      if (typeof callback !== "function") {
        return;
      }
      void context.cursorColor.sampleOnce().then(callback).catch(onError);
    },
    getWindowType(): "main" {
      return "main";
    },
    isDarkColors(): boolean {
      return typeof window.matchMedia === "function"
        && window.matchMedia("(prefers-color-scheme: dark)").matches;
    },
    isWindows(): boolean {
      return /\bwindows?\b|\bwin(?:32|64)\b/.test(platformText());
    },
    isMacOS(): boolean {
      const platform = platformText();
      return platform.includes("mac") || platform.includes("darwin");
    },
    isLinux(): boolean {
      return platformText().includes("linux");
    },
  });

  const utoolsDescriptor = Object.getOwnPropertyDescriptor(window, "utools");
  const rubickDescriptor = Object.getOwnPropertyDescriptor(window, "rubick");

  try {
    Object.defineProperties(window, {
      utools: {
        configurable: true,
        enumerable: false,
        value: api,
        writable: false,
      },
      rubick: {
        configurable: true,
        enumerable: false,
        value: api,
        writable: false,
      },
    });
  } catch (error) {
    if (window.utools === api) {
      if (utoolsDescriptor) {
        Object.defineProperty(window, "utools", utoolsDescriptor);
      } else {
        delete window.utools;
      }
    }
    if (window.rubick === api) {
      if (rubickDescriptor) {
        Object.defineProperty(window, "rubick", rubickDescriptor);
      } else {
        delete window.rubick;
      }
    }
    onError(error);
    return noopDisposable;
  }

  let disposed = false;
  return {
    dispose() {
      if (disposed) {
        return;
      }
      disposed = true;

      if (window.utools === api) {
        if (utoolsDescriptor) {
          Object.defineProperty(window, "utools", utoolsDescriptor);
        } else {
          delete window.utools;
        }
      }
      if (window.rubick === api) {
        if (rubickDescriptor) {
          Object.defineProperty(window, "rubick", rubickDescriptor);
        } else {
          delete window.rubick;
        }
      }
    },
  };
}
