import assert from "node:assert/strict";
import test from "node:test";

import {
  bootstrapPlugin,
  createDevelopmentBridge,
} from "../dist/runtime.js";

test("dynamic command registration never forwards artwork or OS shortcuts to the host", async () => {
  const calls = [];
  const bridge = {
    async call(request) {
      calls.push(request);
      return undefined;
    },
    async listen() {
      return () => undefined;
    },
  };

  const runtime = await bootstrapPlugin(
    "ihub-plugin-test",
    async (context) => {
      await context.commands.register(
        {
          id: "dynamic",
          title: "Dynamic",
          subtitle: "Registered at runtime",
          keywords: ["test"],
          icon: "../../local-secret.png",
          shortcut: "Alt+KeyP",
        },
        async () => ({ close: false }),
      );
    },
    { bridge },
  );

  const registration = calls.find((request) => request.method === "commands.register");
  assert.ok(registration);
  assert.deepEqual(registration.params.definition, {
    id: "dynamic",
    title: "Dynamic",
    subtitle: "Registered at runtime",
    keywords: ["test"],
  });
  assert.equal("icon" in registration.params.definition, false);
  assert.equal("shortcut" in registration.params.definition, false);

  await runtime.dispose();
});

test("sub-input callbacks stay inside the SDK runtime and follow host lifecycle", async () => {
  const bridge = createDevelopmentBridge();
  const changes = [];
  let context;
  const runtime = await bootstrapPlugin(
    "ihub-plugin-sub-input",
    (pluginContext) => {
      context = pluginContext;
    },
    { bridge },
  );

  assert.equal(
    await context.subInput.set(
      ({ text }) => {
        changes.push(text);
      },
      "Search safely",
      false,
    ),
    true,
  );
  await bridge.emit(
    "ihub://plugin/ihub-plugin-sub-input/event/subInput.change",
    { text: "typed" },
  );
  assert.deepEqual(changes, ["typed"]);

  assert.equal(await context.subInput.setValue("programmatic"), true);
  assert.deepEqual(changes, ["typed", "programmatic"]);
  assert.equal(await context.subInput.remove(), true);
  assert.equal(await context.subInput.setValue("ignored"), false);
  assert.deepEqual(changes, ["typed", "programmatic"]);

  await assert.rejects(
    context.subInput.set(() => undefined, "x".repeat(161)),
    /at most 160 characters/,
  );
  await assert.rejects(
    context.subInput.setValue("x".repeat(4_097)),
    /at most 4096 characters/,
  );

  await runtime.dispose();
});

test("bootstrap installs only the bounded uTools and Rubick compatibility projection", async () => {
  const previousWindowDescriptor = Object.getOwnPropertyDescriptor(globalThis, "window");
  const calls = [];
  const listeners = new Map();
  const errors = [];
  const fakeWindow = {
    matchMedia(query) {
      return { matches: query.includes("dark") };
    },
  };
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: fakeWindow,
    writable: true,
  });

  const bridge = {
    async call(request) {
      calls.push(request);
      if (
        request.method === "ui.subInput.set"
        || request.method === "ui.subInput.remove"
        || request.method === "ui.subInput.setValue"
      ) {
        return true;
      }
      if (request.method === "cursorColor.sampleOnce") {
        return { hex: "#112233", rgb: "rgb(17, 34, 51)" };
      }
      return undefined;
    },
    async listen(name, listener) {
      listeners.set(name, listener);
      return () => listeners.delete(name);
    },
  };

  try {
    const runtime = await bootstrapPlugin(
      "ihub-plugin-compat",
      () => undefined,
      {
        bridge,
        onError(error) {
          errors.push(error);
        },
      },
    );

    assert.ok(fakeWindow.utools);
    assert.equal(fakeWindow.utools, fakeWindow.rubick);
    assert.equal(Object.isFrozen(fakeWindow.utools), true);
    assert.deepEqual(Object.keys(fakeWindow.utools).sort(), [
      "copyText",
      "getWindowType",
      "isDarkColors",
      "isLinux",
      "isMacOS",
      "isWindows",
      "removeSubInput",
      "screenColorPick",
      "setSubInput",
      "setSubInputValue",
      "shellOpenExternal",
      "shellOpenPath",
      "showNotification",
    ]);
    for (const forbidden of [
      "db",
      "fs",
      "child_process",
      "remote",
      "require",
      "createBrowserWindow",
      "getPath",
      "showOpenDialog",
      "simulateKeyboardTap",
    ]) {
      assert.equal(forbidden in fakeWindow.utools, false);
    }

    assert.equal(fakeWindow.utools.setSubInput(() => undefined, "Filter", false), true);
    assert.equal(fakeWindow.utools.setSubInputValue("query"), true);
    assert.equal(fakeWindow.utools.copyText("copy"), true);
    fakeWindow.utools.showNotification("done");
    fakeWindow.utools.shellOpenExternal("https://example.com");
    fakeWindow.utools.shellOpenPath("user-selected.txt");
    let sampledColor;
    fakeWindow.utools.screenColorPick((color) => {
      sampledColor = color;
    });
    await new Promise((resolve) => setImmediate(resolve));
    await new Promise((resolve) => setImmediate(resolve));

    assert.deepEqual(sampledColor, {
      hex: "#112233",
      rgb: "rgb(17, 34, 51)",
    });
    assert.equal(fakeWindow.utools.getWindowType(), "main");
    assert.equal(fakeWindow.utools.isDarkColors(), true);
    assert.ok(calls.some((request) => request.method === "ui.subInput.set"));
    assert.ok(calls.some((request) => request.method === "ui.subInput.setValue"));
    assert.ok(
      calls.findIndex((request) => request.method === "ui.subInput.set")
      < calls.findIndex((request) => request.method === "ui.subInput.setValue"),
    );
    assert.ok(calls.some((request) => request.method === "clipboard.writeText"));
    assert.ok(calls.some((request) => request.method === "notifications.show"));
    assert.ok(calls.some((request) => request.method === "shell.openExternal"));
    assert.ok(calls.some((request) => request.method === "shell.openPath"));
    assert.ok(calls.some((request) => request.method === "cursorColor.sampleOnce"));
    assert.deepEqual(errors, []);

    await runtime.dispose();
    assert.equal(fakeWindow.utools, undefined);
    assert.equal(fakeWindow.rubick, undefined);

    const existingHostApi = Object.freeze({ existing: true });
    Object.defineProperty(fakeWindow, "utools", {
      configurable: true,
      value: existingHostApi,
      writable: true,
    });
    const preservedRuntime = await bootstrapPlugin(
      "ihub-plugin-preserve-existing-host",
      () => undefined,
      { bridge },
    );
    assert.equal(fakeWindow.utools, existingHostApi);
    assert.equal(fakeWindow.rubick, undefined);
    await preservedRuntime.dispose();
    assert.equal(fakeWindow.utools, existingHostApi);
    delete fakeWindow.utools;
  } finally {
    if (previousWindowDescriptor) {
      Object.defineProperty(globalThis, "window", previousWindowDescriptor);
    } else {
      delete globalThis.window;
    }
  }
});
