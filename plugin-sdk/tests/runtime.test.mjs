import assert from "node:assert/strict";
import test from "node:test";

import { bootstrapPlugin } from "../dist/runtime.js";

test("dynamic command registration never forwards an icon to the host", async () => {
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

  await runtime.dispose();
});
