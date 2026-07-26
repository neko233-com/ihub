# iHub Plugin SDK

`@ihub/plugin-sdk` is the small TypeScript boundary between a plugin frontend and the iHub desktop host. It has no React, Node.js, or Tauri package dependency; a plugin can therefore ship a compact Vite bundle and still receive typed commands, search requests, settings, clipboard, shell, and process APIs.

```ts
import { bootstrapPlugin } from "@ihub/plugin-sdk";

await bootstrapPlugin("ihub-plugin-example", async (ihub) => {
  await ihub.commands.register(
    { id: "hello", title: "Say hello" },
    () => ({ message: "Hello from iHub", close: true }),
  );
});
```

Use `manifest.schema.json` in editor validation and read [`../docs/PLUGIN_DEVELOPMENT.md`](../docs/PLUGIN_DEVELOPMENT.md) for the package, binary-backend, permission, and GitHub import contracts.

The SDK is an ergonomic IPC client, not a sandbox. A plugin with a native binary can execute with the installing user's privileges; iHub surfaces declared capabilities and package provenance, but users must install only code they trust.
