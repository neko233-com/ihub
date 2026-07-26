import {
  bootstrapPlugin,
  createDevelopmentBridge,
  hasIHubHost,
  type PluginContext,
  type SearchResult,
} from "@ihub/plugin-sdk";
import manifest from "../plugin.json";
import "./style.css";

const app = document.querySelector<HTMLDivElement>("#app");

if (!app) {
  throw new Error("The plugin root element is missing.");
}

const isPreview = !hasIHubHost();
const previewBridge = isPreview ? createDevelopmentBridge() : undefined;

app.innerHTML = `
  <section class="card" aria-labelledby="title">
    <div class="eyebrow"><span class="dot"></span>${isPreview ? "Browser preview" : "Connected to iHub"}</div>
    <h1 id="title">Hello, iHub.</h1>
    <p class="lede">A dependency-light TypeScript/Vite plugin. Its command and search provider are registered through the SDK.</p>
    <label class="field" for="name">Greeting name</label>
    <div class="row">
      <input id="name" name="name" autocomplete="name" value="iHub user" />
      <button id="greet" type="button">Greet</button>
    </div>
    <p id="message" class="message" role="status">Ready.</p>
    <p class="hint">Try <kbd>hello</kbd> in iHub search after loading the built plugin.</p>
  </section>
`;

const nameInput = document.querySelector<HTMLInputElement>("#name");
const greetButton = document.querySelector<HTMLButtonElement>("#greet");
const message = document.querySelector<HTMLParagraphElement>("#message");

if (!nameInput || !greetButton || !message) {
  throw new Error("The Hello plugin UI did not initialize.");
}

const nameField = nameInput;
const greetButtonElement = greetButton;
const messageNode = message;

function greeting(name: string): string {
  const trimmed = name.trim();
  return `Hello, ${trimmed || "iHub user"}!`;
}

function showGreeting(name = nameField.value): string {
  const text = greeting(name);
  messageNode.textContent = text;
  return text;
}

greetButtonElement.addEventListener("click", () => showGreeting());

async function activate(context: PluginContext): Promise<void> {
  const savedName = await context.settings.get<string>("displayName", "iHub user");
  nameField.value = savedName;

  nameField.addEventListener("change", () => {
    void context.settings.set("displayName", nameField.value.trim() || "iHub user");
  });

  await context.commands.register(
    {
      id: "hello",
      title: "Say hello",
      subtitle: "Show a greeting from the Hello plugin",
      keywords: ["hello", "hi", "example"],
    },
    async () => {
      const text = showGreeting();
      await context.notifications.show({ title: "Hello iHub", body: text, level: "success" });
      return { message: text, close: true };
    },
  );

  await context.search.register(
    { id: "hello-search", title: "Hello examples", trigger: "hello ", priority: 20 },
    (request): SearchResult[] => {
      const query = request.query.trim();
      return [
        {
          id: "greet",
          title: greeting(query || nameField.value),
          subtitle: "Run the Say hello command",
          score: query ? 1 : 0.8,
          payload: { name: query || nameField.value },
          actions: [{ id: "run", title: "Greet" }],
        },
      ];
    },
  );

  context.logger.info("Hello plugin activated", { preview: isPreview });
}

void bootstrapPlugin(manifest.id, activate, {
  bridge: previewBridge,
  onError(error) {
    messageNode.textContent = `Plugin error: ${error instanceof Error ? error.message : String(error)}`;
    console.error(error);
  },
});
