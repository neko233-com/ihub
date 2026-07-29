import type {
  CommandDefinition,
  PluginContext,
  PluginManifest,
  RuntimeCommandDefinition,
} from "../src/types.js";

const staticCommand: CommandDefinition = {
  id: "static",
  title: "Static command",
  icon: "public/static.png",
};
void staticCommand;

const runtimeCommand: RuntimeCommandDefinition = {
  id: "runtime",
  title: "Runtime command",
};
void runtimeCommand;

const microphoneManifest: PluginManifest = {
  schemaVersion: 1,
  id: "ihub-plugin-microphone-contract",
  name: "Microphone contract",
  version: "1.0.0",
  engines: { ihub: ">=0.1.0", api: "^1.0.0" },
  entry: { frontend: "dist/index.html" },
  permissions: { microphone: true },
};
void microphoneManifest;

const invalidMicrophoneManifest: PluginManifest = {
  ...microphoneManifest,
  permissions: {
    // @ts-expect-error Microphone delegation is an explicit boolean permission.
    microphone: "yes",
  },
};
void invalidMicrophoneManifest;

declare const context: PluginContext;
void context.subInput.set(({ text }) => {
  const value: string = text;
  void value;
}, "Search", false);
void context.subInput.setValue("query");
void context.subInput.remove();

void context.commands.register(
  {
    id: "dynamic",
    title: "Dynamic command",
    // @ts-expect-error Runtime registration cannot send artwork to the host.
    icon: "public/dynamic.png",
  },
  async () => ({ close: false }),
);

window.utools?.setSubInput(({ text }) => {
  const value: string = text;
  void value;
}, "Search", true);
window.rubick?.setSubInputValue("query");
window.utools?.copyText("safe text");
window.utools?.shellOpenExternal("https://example.com");

// @ts-expect-error The compatibility projection never exposes Node.js.
window.utools?.require("fs");
// @ts-expect-error The compatibility projection never exposes raw filesystem APIs.
window.rubick?.fs.readFile("secret.txt");
// @ts-expect-error The compatibility projection never exposes Electron remote.
window.utools?.remote.getCurrentWindow();
// @ts-expect-error Arbitrary process spawning is not a compatibility API.
window.rubick?.child_process.spawn("cmd.exe");
