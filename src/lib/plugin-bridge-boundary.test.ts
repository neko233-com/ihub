import { describe, expect, it } from "vitest";
import {
  PLUGIN_BRIDGE_MAX_IN_FLIGHT,
  PLUGIN_BRIDGE_MAX_DB_JSON_BYTES,
  PLUGIN_BRIDGE_MAX_IMAGE_DATA_URL_CHARS,
  PLUGIN_BRIDGE_MAX_JSON_BYTES,
  PLUGIN_BRIDGE_MAX_JSON_DEPTH,
  PluginBridgeInFlightGate,
  isLargePluginBridgeMethod,
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
    expect(validatePluginBridgeCall(call("compatibility.utools.clipboard.writeImage", {
      dataUrl: "data:image/png;base64,iVBORw0KGgo=",
    })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.clipboard.writeFiles", {
      paths: ["C:\\Users\\Tester\\Desktop\\notes.txt"],
    })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.dbStorage.snapshot", {})).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.dbStorage.set", { key: "theme", value: "dark" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.dbStorage.remove", { key: "theme" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.db.get", { id: "note/1" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.db.put", { doc: { _id: "note/1", text: "hello" } })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.db.remove", { target: "note/1" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.db.bulkDocs", { docs: [{ _id: "note/1" }] })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.db.allDocs", { selector: "note/" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.db.allDocs", {
      selector: Array.from({ length: 256 }, (_, index) => `document-${index}-${"x".repeat(480)}`),
    })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.db.postAttachment", {
      id: "asset/logo",
      dataBase64: "c2FmZQ==",
      contentType: "text/plain",
    })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.db.getAttachment", { id: "asset/logo" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.db.getAttachmentType", { id: "asset/logo" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.features.snapshot", {})).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.features.set", { feature: { code: "docs", cmds: ["文档"] } })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.features.remove", { code: "docs" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.input.pasteText", { value: "粘贴" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.input.pasteFiles", { paths: ["C:\\Users\\Tester\\Desktop\\notes.txt"] })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.input.pasteImage", { dataUrl: "data:image/png;base64,iVBORw0KGgo=" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.input.typeString", { value: "type" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.shell.beep", {})).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.notification.show", { body: "done" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.screen.capture", {})).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.screen.capture", { displayIndex: 1 })).ok).toBe(false);
    expect(validatePluginBridgeCall(call("compatibility.utools.shell.openExternal", { url: "https://example.com" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.shell.openPath", { path: "C:\\Users\\Tester\\notes.txt" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.shell.showItemInFolder", { path: "C:\\Users\\Tester\\notes.txt" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.shell.trashItem", { path: "C:\\Users\\Tester\\notes.txt" })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.system.readCurrentFolderPath", {})).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.system.readCurrentBrowserUrl", {})).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.window.setHeight", { height: 300 })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.window.outPlugin", { isKill: false })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.window.redirect", {
      label: ["Translate", "翻译"],
      action: { type: "text", payload: "hello" },
    })).ok).toBe(true);
    expect(validatePluginBridgeCall(call(), "com.example.safe").ok).toBe(true);
    expect(validatePluginBridgeCall(call(), "com.example.other").ok).toBe(false);
    expect(validatePluginBridgeCall(call("process.spawn")).ok).toBe(false);
    expect(validatePluginBridgeCall({ ...call(), surprise: true }).ok).toBe(false);
    expect(validatePluginBridgeCall({
      ...call(),
      request: { ...call().request, arbitrary: true },
    }).ok).toBe(false);
  });

  it("enlarges only a shape-checked PNG copy request", () => {
    const boundedImage = "data:image/png;base64,iVBORw0KGgo".padEnd(
      PLUGIN_BRIDGE_MAX_IMAGE_DATA_URL_CHARS,
      "A",
    );
    expect(validatePluginBridgeCall(call("compatibility.utools.clipboard.writeImage", {
      dataUrl: boundedImage,
    })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.clipboard.writeImage", {
      dataUrl: `${boundedImage}A`,
    })).ok).toBe(false);
    expect(validatePluginBridgeCall(call("compatibility.utools.clipboard.writeImage", {
      dataUrl: "C:\\untrusted\\image.png",
    })).ok).toBe(false);
    expect(validatePluginBridgeCall(call("compatibility.utools.clipboard.writeImage", {
      dataUrl: "data:image/jpeg;base64,/9j/",
    })).ok).toBe(false);
    expect(validatePluginBridgeCall(call("compatibility.utools.window.redirect", {
      label: "OCR",
      action: { type: "img", payload: boundedImage },
    })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.window.redirect", {
      label: "OCR",
      action: { type: "img", payload: `${boundedImage}A` },
    })).ok).toBe(false);
    expect(validatePluginBridgeCall(call("compatibility.utools.window.redirect", {
      label: [],
      action: { type: "files", payload: [] },
    })).ok).toBe(false);
    expect(validatePluginBridgeCall(call("log", {
      message: "A".repeat(PLUGIN_BRIDGE_MAX_JSON_BYTES),
    })).ok).toBe(false);
  });

  it("reserves the large JSON envelope for shape-checked document writes", () => {
    const text = "x".repeat(PLUGIN_BRIDGE_MAX_JSON_BYTES);
    expect(validatePluginBridgeCall(call("compatibility.utools.db.put", {
      doc: { _id: "large", text },
    })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.db.bulkDocs", {
      docs: [{ _id: "large", text }],
    })).ok).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.db.put", {
      doc: { _id: "large", text: "x".repeat(PLUGIN_BRIDGE_MAX_DB_JSON_BYTES) },
    })).ok).toBe(false);
    expect(validatePluginBridgeCall(call("compatibility.utools.db.put", {
      doc: [],
    })).ok).toBe(false);
    expect(validatePluginBridgeCall(call("compatibility.utools.db.bulkDocs", {
      docs: [],
    })).ok).toBe(false);
    expect(validatePluginBridgeCall(call("log", { message: text })).ok).toBe(false);
    expect(isLargePluginBridgeMethod("compatibility.utools.db.put")).toBe(true);
    expect(isLargePluginBridgeMethod("compatibility.utools.db.bulkDocs")).toBe(true);
    expect(isLargePluginBridgeMethod("compatibility.utools.db.allDocs")).toBe(false);
    expect(isLargePluginBridgeMethod("compatibility.utools.db.postAttachment")).toBe(true);
    expect(isLargePluginBridgeMethod("compatibility.utools.input.pasteImage")).toBe(true);
    expect(isLargePluginBridgeMethod("compatibility.utools.window.redirect")).toBe(true);
    expect(validatePluginBridgeCall(call("compatibility.utools.db.postAttachment", {
      id: "asset/logo",
      dataBase64: "c2FmZQ==",
      contentType: "text/plain",
      extra: true,
    })).ok).toBe(false);
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
    expect(gate.begin("image-1", true)).toBe("accepted");
    expect(gate.begin("image-2", true)).toBe("busy");
    expect(gate.begin("ordinary-while-image")).toBe("accepted");
    gate.finish("image-1");
    expect(gate.begin("image-2", true)).toBe("accepted");
    gate.clear();
    expect(gate.size).toBe(0);
  });
});
