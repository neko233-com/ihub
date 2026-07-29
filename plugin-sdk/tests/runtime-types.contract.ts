import type {
  CommandDefinition,
  PluginContext,
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

declare const context: PluginContext;
void context.commands.register(
  {
    id: "dynamic",
    title: "Dynamic command",
    // @ts-expect-error Runtime registration cannot send artwork to the host.
    icon: "public/dynamic.png",
  },
  async () => ({ close: false }),
);
