import { describe, expect, it } from "vitest";
import {
  PLUGIN_BRIDGE_MAX_IN_FLIGHT,
  PLUGIN_BRIDGE_MAX_JSON_BYTES,
  PLUGIN_BRIDGE_MAX_JSON_DEPTH,
  PluginBridgeInFlightGate,
  validatePluginBridgeCall,
} from "./plugin-bridge-boundary";

function call(method = "lifecycle.ready", params: unknown = null) {
  return {
    channel: "ihub-plugin-bridge/v1",
    type: "call",
    id: "frame-safe-1",
    request: { pluginId: "com.example.safe", method, params },
  };
}

describe("plugin iframe Bridge boundary", () => {
  it("accepts only the fixed host method contract and exact envelope", () => {
    expect(validatePluginBridgeCall(call()).ok).toBe(true);
    expect(validatePluginBridgeCall(call("ui.subInput.remove", undefined)).ok).toBe(true);
    expect(validatePluginBridgeCall(call("ui.subInput.select", {})).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.clipboard.writeText", { value: "copied" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.dbStorage.snapshot", {})).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.dbStorage.set", { key: "theme", value: "dark" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.dbStorage.remove", { key: "theme" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.features.snapshot", {})).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.features.set", { feature: { code: "docs", cmds: ["文档"] } })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.features.remove", { code: "docs" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.input.pasteText", { value: "粘贴" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.input.typeString", { value: "type" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.shell.beep", {})).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.notification.show", { body: "done" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.shell.openExternal", { url: "https://example.com" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.window.setHeight", { height: 300 })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.window.outPlugin", { isKill: false })).ok).toBe(true);
    expect(validatePluginBridgeCall(call(), "com.example.safe").ok).toBe(true);
    expect(validatePluginBridgeCall(call(), "com.example.other").ok).toBe(false);
    expect(validatePluginBridgeCall(call("process.spawn")).ok).toBe(false);
    expect(validatePluginBridgeCall({ ...call(), surprise: true }).ok).toBe(false);
    expect(validatePluginBridgeCall({
      ...call(),
      request: { ...call().request, arbitrary: true },
    }).ok).toBe(false);
  });

  it("rejects oversized, too-deep, cyclic, and non-JSON payloads iteratively", () => {
    expect(validatePluginBridgeCall(call("log", {
      message: "x".repeat(PLUGIN_BRIDGE_MAX_JSON_BYTES),
    })).ok).toBe(false);

    let nested: Record<string, unknown> = {};
    const root = nested;
    for (let depth = 0; depth <= PLUGIN_BRIDGE_MAX_JSON_DEPTH; depth += 1) {
      nested.next = {};
      nested = nested.next as Record<string, unknown>;
    }
    expect(validatePluginBridgeCall(call("log", root)).ok).toBe(false);

    const cyclic: Record<string, unknown> = {};
    cyclic.self = cyclic;
    expect(validatePluginBridgeCall(call("log", cyclic)).ok).toBe(false);
    expect(validatePluginBridgeCall(call("log", { invalid: new Uint8Array(8) })).ok).toBe(false);
  });

  it("caps unique in-flight calls and releases capacity deterministically", () => {
    const gate = new PluginBridgeInFlightGate();
    for (let index = 0; index < PLUGIN_BRIDGE_MAX_IN_FLIGHT; index += 1) {
      expect(gate.begin(`call-${index}`)).toBe("accepted");
    }
    expect(gate.begin("call-0")).toBe("duplicate");
    expect(gate.begin("overflow")).toBe("busy");
    gate.finish("call-0");
    expect(gate.begin("overflow")).toBe("accepted");
    gate.clear();
    expect(gate.size).toBe(0);
  });
});
