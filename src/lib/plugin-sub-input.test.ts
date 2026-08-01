import { describe, expect, it } from "vitest";
import {
  PLUGIN_SUB_INPUT_MAX_PLACEHOLDER_LENGTH,
  PLUGIN_SUB_INPUT_MAX_VALUE_LENGTH,
  parsePluginSubInputBridgeCall,
  resolvePluginSubInputBridgeCall,
} from "./plugin-sub-input";

describe("plugin sub-input bridge parser", () => {
  it("accepts only the renderer-owned methods with bounded JSON values", () => {
    expect(parsePluginSubInputBridgeCall("ui.subInput.set", undefined)).toEqual({
      handled: true,
      ok: true,
      action: { kind: "set", placeholder: "", focus: true },
    });
    expect(parsePluginSubInputBridgeCall("ui.subInput.set", {
      placeholder: "搜索",
      focus: false,
    })).toEqual({
      handled: true,
      ok: true,
      action: { kind: "set", placeholder: "搜索", focus: false },
    });
    expect(parsePluginSubInputBridgeCall("ui.subInput.setValue", {
      value: "needle",
    })).toEqual({
      handled: true,
      ok: true,
      action: { kind: "set-value", value: "needle" },
    });
    expect(parsePluginSubInputBridgeCall("ui.subInput.remove", {})).toEqual({
      handled: true,
      ok: true,
      action: { kind: "remove" },
    });
    expect(parsePluginSubInputBridgeCall("ui.subInput.focus", {})).toEqual({
      handled: true,
      ok: true,
      action: { kind: "focus" },
    });
    expect(parsePluginSubInputBridgeCall("ui.subInput.blur", {})).toEqual({
      handled: true,
      ok: true,
      action: { kind: "blur" },
    });
    expect(parsePluginSubInputBridgeCall("ui.subInput.select", {})).toEqual({
      handled: true,
      ok: true,
      action: { kind: "select" },
    });
    expect(parsePluginSubInputBridgeCall("shell.openExternal", {})).toEqual({
      handled: false,
    });
  });

  it("binds state transitions to a visible surface and reports callback events", () => {
    expect(resolvePluginSubInputBridgeCall(
      null,
      "ui.subInput.set",
      { placeholder: "Filter", focus: true },
      true,
    )).toEqual({
      handled: true,
      ok: false,
      error: "Sub-input controls are unavailable from a hidden plugin runtime.",
    });

    const created = resolvePluginSubInputBridgeCall(
      null,
      "ui.subInput.set",
      { placeholder: "Filter", focus: true },
      false,
    );
    expect(created).toEqual({
      handled: true,
      ok: true,
      result: true,
      state: {
        focusVersion: 1,
        placeholder: "Filter",
        selectionVersion: 0,
        value: "",
      },
    });
    if (!created.handled || !created.ok) {
      throw new Error("Expected a created sub-input state.");
    }

    expect(resolvePluginSubInputBridgeCall(
      created.state,
      "ui.subInput.setValue",
      { value: "needle" },
      false,
    )).toEqual({
      handled: true,
      ok: true,
      result: true,
      state: {
        focusVersion: 1,
        placeholder: "Filter",
        selectionVersion: 0,
        value: "needle",
      },
      emitText: "needle",
    });
    expect(resolvePluginSubInputBridgeCall(
      created.state,
      "ui.subInput.select",
      {},
      false,
    )).toMatchObject({
      handled: true,
      ok: true,
      result: true,
      state: { focusVersion: 2, selectionVersion: 1 },
    });
    expect(resolvePluginSubInputBridgeCall(
      created.state,
      "ui.subInput.blur",
      {},
      false,
    )).toMatchObject({ handled: true, ok: true, result: true, focusPluginFrame: true });
    expect(resolvePluginSubInputBridgeCall(
      created.state,
      "ui.subInput.remove",
      undefined,
      false,
    )).toEqual({
      handled: true,
      ok: true,
      result: true,
      state: null,
      focusPluginFrame: true,
    });
    expect(resolvePluginSubInputBridgeCall(
      null,
      "ui.subInput.setValue",
      { value: "orphan" },
      false,
    )).toMatchObject({
      handled: true,
      ok: true,
      result: false,
      state: null,
    });
  });

  it("rejects ambiguous, oversized, and capability-shaped payloads", () => {
    expect(parsePluginSubInputBridgeCall("ui.subInput.set", {
      placeholder: "ok",
      path: "C:\\secret",
    })).toMatchObject({ handled: true, error: expect.any(String) });
    expect(parsePluginSubInputBridgeCall("ui.subInput.set", {
      placeholder: "x".repeat(PLUGIN_SUB_INPUT_MAX_PLACEHOLDER_LENGTH + 1),
    })).toMatchObject({ handled: true, error: expect.any(String) });
    expect(parsePluginSubInputBridgeCall("ui.subInput.setValue", {
      value: "x".repeat(PLUGIN_SUB_INPUT_MAX_VALUE_LENGTH + 1),
    })).toMatchObject({ handled: true, error: expect.any(String) });
    expect(parsePluginSubInputBridgeCall("ui.subInput.remove", {
      command: "child_process",
    })).toMatchObject({ handled: true, error: expect.any(String) });
    expect(parsePluginSubInputBridgeCall("ui.subInput.setValue", ["value"]))
      .toMatchObject({ handled: true, error: expect.any(String) });
    expect(parsePluginSubInputBridgeCall("ui.subInput.focus", { force: true }))
      .toMatchObject({ handled: true, error: expect.any(String) });
  });
});
