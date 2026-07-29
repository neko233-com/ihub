import { describe, expect, it } from "vitest";
import {
  createDetachedPluginShortcutEvent,
  isValidPluginId,
  parseApplicationRoute,
  shouldDetachPluginSurface,
  type PluginSurfaceShortcutEvent,
} from "./detached-plugin-window";
import type { PluginInfo } from "./types";

const ctrlD: PluginSurfaceShortcutEvent = {
  altKey: false,
  ctrlKey: true,
  defaultPrevented: false,
  isComposing: false,
  key: "d",
  metaKey: false,
  repeat: false,
  shiftKey: false,
};

describe("detached plugin window routing", () => {
  it("accepts only the fixed native route and explicit browser preview", () => {
    expect(parseApplicationRoute("", true)).toEqual({ kind: "main" });
    expect(
      parseApplicationRoute("?ihubDetachedPlugin=com.example.notes", true),
    ).toEqual({
      kind: "detached",
      pluginId: "com.example.notes",
      browserPreview: false,
    });
    expect(
      parseApplicationRoute(
        "?ihubDetachedPlugin=com.example.notes&ihubDetachedPreview=1",
        false,
      ),
    ).toEqual({
      kind: "detached",
      pluginId: "com.example.notes",
      browserPreview: true,
    });
  });

  it("fails closed for URL, path, duplicate, unknown, and desktop-preview input", () => {
    for (const search of [
      "?ihubDetachedPlugin=https%3A%2F%2Fexample.com",
      "?ihubDetachedPlugin=..%2Fescape",
      "?ihubDetachedPlugin=x",
      "?ihubDetachedPlugin=one&ihubDetachedPlugin=two",
      "?ihubDetachedPlugin=com.example.notes&url=https%3A%2F%2Fexample.com",
      "?ihubDetachedPlugin=com.example.notes&ihubDetachedPreview=yes",
    ]) {
      expect(parseApplicationRoute(search, false).kind).toBe(
        "invalid-detached",
      );
    }
    expect(
      parseApplicationRoute(
        "?ihubDetachedPlugin=com.example.notes&ihubDetachedPreview=1",
        true,
      ).kind,
    ).toBe("invalid-detached");
    expect(
      parseApplicationRoute(
        "?ihubDetachedPlugin=com.example.notes",
        false,
        "#https://example.com",
      ).kind,
    ).toBe("invalid-detached");
  });

  it("uses the same bounded ASCII plugin ID grammar as the native host", () => {
    expect(isValidPluginId("com.example_plugin-2")).toBe(true);
    expect(isValidPluginId("x")).toBe(false);
    expect(isValidPluginId("plugin/escape")).toBe(false);
    expect(isValidPluginId(`x${"a".repeat(96)}`)).toBe(false);
  });
});

describe("detached plugin shortcut", () => {
  it("handles Ctrl+D only for an active trusted plugin surface", () => {
    expect(shouldDetachPluginSurface(ctrlD, true)).toBe(true);
    expect(shouldDetachPluginSurface(ctrlD, false)).toBe(false);
    expect(
      shouldDetachPluginSurface({ ...ctrlD, defaultPrevented: true }, true),
    ).toBe(false);
    expect(
      shouldDetachPluginSurface({ ...ctrlD, isComposing: true }, true),
    ).toBe(false);
    expect(
      shouldDetachPluginSurface({ ...ctrlD, repeat: true }, true),
    ).toBe(false);
    expect(
      shouldDetachPluginSurface({ ...ctrlD, key: "k" }, true),
    ).toBe(false);
    expect(
      shouldDetachPluginSurface({ ...ctrlD, ctrlKey: false, metaKey: true }, true),
    ).toBe(false);
    expect(
      shouldDetachPluginSurface({ ...ctrlD, shiftKey: true }, true),
    ).toBe(false);
  });

  it("routes only the exact detached plugin frontend command", () => {
    const plugin: PluginInfo = {
      id: "com.example.notes",
      name: "Notes",
      version: "1.0.0",
      enabled: true,
      frontendEntry: "dist/index.html",
      hasNativeWorker: false,
      commands: [{
        id: "open",
        name: "Open",
        execution: "frontend",
      }],
    };
    expect(
      createDetachedPluginShortcutEvent(
        plugin.id,
        plugin,
        {
          pluginId: plugin.id,
          shortcut: "Alt+KeyN",
          commandId: "open",
        },
        "detached-shortcut-test",
      ),
    ).toEqual({
      id: "detached-shortcut-test",
      pluginId: plugin.id,
      name: `ihub://plugin/${plugin.id}/command`,
      payload: {
        requestId: "detached-shortcut-test",
        commandId: "open",
        input: null,
        context: null,
      },
    });
  });

  it("never redirects mismatched, keyword, stale, or native commands", () => {
    const plugin: PluginInfo = {
      id: "com.example.notes",
      name: "Notes",
      version: "1.0.0",
      enabled: true,
      frontendEntry: "dist/index.html",
      hasNativeWorker: true,
      commands: [{
        id: "open",
        name: "Open",
        execution: "native",
      }],
    };
    const shortcut = {
      pluginId: plugin.id,
      shortcut: "Alt+KeyN",
      commandId: "open",
    };
    expect(
      createDetachedPluginShortcutEvent("com.example.other", plugin, shortcut),
    ).toBeNull();
    expect(
      createDetachedPluginShortcutEvent(plugin.id, plugin, {
        pluginId: plugin.id,
        shortcut: "Alt+KeyN",
        keyword: "notes",
      }),
    ).toBeNull();
    expect(
      createDetachedPluginShortcutEvent(plugin.id, plugin, {
        ...shortcut,
        commandId: "removed",
      }),
    ).toBeNull();
    expect(
      createDetachedPluginShortcutEvent(plugin.id, plugin, shortcut),
    ).toBeNull();
  });
});
