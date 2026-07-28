import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import {
  availableLauncherContextActions,
  deriveLauncherContextActions,
} from "./launcher-context-actions";
import { eligibleLauncherContextCommands } from "./plugin-launcher-context";
import type { PluginInfo } from "./types";

interface OfficialManifest {
  id: string;
  name: string;
  version: string;
  entry?: { frontend?: string };
  contributes?: {
    commands?: Array<{
      id?: string;
      title?: string;
      subtitle?: string;
      execution?: string;
    }>;
  };
  permissions?: {
    launcherContext?: {
      text?: boolean;
      files?: boolean;
      image?: boolean;
    };
  };
}

const workspaceRoot = fileURLToPath(new URL("../../", import.meta.url));

function readOfficialPlugin(id: string): PluginInfo {
  const manifest = JSON.parse(readFileSync(
    resolve(workspaceRoot, "plugins", "official", id, "plugin.json"),
    "utf8",
  )) as OfficialManifest;
  return {
    id: manifest.id,
    name: manifest.name,
    version: manifest.version,
    frontendEntry: manifest.entry?.frontend,
    enabled: true,
    launcherContext: manifest.permissions?.launcherContext,
    commands: (manifest.contributes?.commands ?? []).flatMap((command) =>
      command.id && command.title
        ? [{
          id: command.id,
          name: command.title,
          description: command.subtitle,
          execution: command.execution === "native" ? "native" as const : "frontend" as const,
        }]
        : [],
    ),
  };
}

describe("launcher context actions", () => {
  it("recognizes JSON without treating an ordinary command search as a text object", () => {
    expect(deriveLauncherContextActions({ query: "json" })).toEqual([]);

    const actions = deriveLauncherContextActions({
      query: '{"project":"iHub","fast":true}',
    });
    expect(actions.map((action) => action.id)).toEqual(["ihub.context.json"]);
    expect(actions[0]?.target).toEqual({
      kind: "builtin",
      commandId: "ihub.tool.json",
      jsonInput: '{"project":"iHub","fast":true}',
    });
  });

  it("offers an explicit text-handoff picker for prose without staging it", () => {
    const actions = deriveLauncherContextActions({
      query: "Please translate this deliberately long sentence into Chinese.",
    });

    expect(actions.map((action) => action.id)).toEqual([
      "ihub.context.translate",
      "ihub.context.text-tools",
    ]);
    expect(actions.every((action) => action.target.kind === "plugin-handoff")).toBe(true);
    expect(actions.map((action) => action.target)).toEqual([
      {
        kind: "plugin-handoff",
        category: "text",
        suggestedUse: "Translate",
        preferredPluginId: "ihub-plugin-translate",
        preferredCommandId: "translate-launcher-text",
      },
      {
        kind: "plugin-handoff",
        category: "text",
        suggestedUse: "Text Tools",
        preferredPluginId: "ihub-plugin-text-tools",
        preferredCommandId: "process-launcher-text",
      },
    ]);
  });

  it("prefills batch rename only for a native clipboard folder", () => {
    const actions = deriveLauncherContextActions({
      query: "",
      pastedFiles: [{
        kind: "folder",
        name: "to-sort",
        path: "D:\\Photos\\to-sort",
      }],
    });

    expect(actions.map((action) => action.id)).toEqual([
      "ihub.context.local-search",
      "ihub.context.batch-rename",
    ]);
    expect(actions[1]?.target).toEqual({
      kind: "builtin",
      commandId: "ihub.tool.batch-rename",
      renameDirectory: "D:\\Photos\\to-sort",
    });
  });

  it("keeps image-plugin handoff explicit for a pasted bitmap", () => {
    const actions = deriveLauncherContextActions({
      query: "",
      hasPastedImage: true,
      pastedImageType: "image/png",
    });

    expect(actions.map((action) => action.id)).toEqual([
      "ihub.context.screenshot",
      "ihub.context.ocr",
      "ihub.context.image-tools",
    ]);
    expect(actions[1]?.target).toEqual({
      kind: "plugin-handoff",
      category: "image",
      suggestedUse: "OCR",
      preferredPluginId: "ihub-plugin-ocr",
      preferredCommandId: "recognize-launcher-image",
    });
    expect(actions[1]?.detail).toContain("确认后");
    expect(actions[1]?.detail).toContain("系统选择器中重新选择");
  });

  it("does not advertise a PNG-only host handoff for an unnormalized DOM image", () => {
    for (const pastedImageType of ["image/jpeg", "image/webp", ""]) {
      expect(deriveLauncherContextActions({
        query: "",
        hasPastedImage: true,
        pastedImageType,
      }).map((action) => action.id)).toEqual(["ihub.context.screenshot"]);
    }
  });

  it("offers file metadata only for formats the intended plugins can actually reopen", () => {
    const pngActions = deriveLauncherContextActions({
      query: "",
      pastedFiles: [{ kind: "file", name: "capture.png", path: "D:\\Pictures\\capture.png" }],
    });
    expect(pngActions.map((action) => action.id)).toEqual([
      "ihub.context.local-search",
      "ihub.context.ocr",
      "ihub.context.image-tools",
    ]);
    expect(pngActions.slice(1).every((action) =>
      action.detail.includes("系统选择器中重新选择"),
    )).toBe(true);

    const webpActions = deriveLauncherContextActions({
      query: "",
      pastedFiles: [{ kind: "file", name: "capture.webp", path: "/tmp/capture.webp" }],
    });
    expect(webpActions.map((action) => action.id)).toEqual([
      "ihub.context.local-search",
      "ihub.context.image-tools",
    ]);

    const unsupportedActions = deriveLauncherContextActions({
      query: "",
      pastedFiles: [{ kind: "file", name: "capture.gif", path: "/tmp/capture.gif" }],
    });
    expect(unsupportedActions.map((action) => action.id)).toEqual([
      "ihub.context.local-search",
    ]);
  });

  it("checks every pasted file and never treats an image-named folder as a plugin input", () => {
    const actions = deriveLauncherContextActions({
      query: "",
      pastedFiles: [
        { kind: "file", name: "notes.txt", path: "D:\\Inbox\\notes.txt" },
        { kind: "folder", name: "album.png", path: "D:\\Inbox\\album.png" },
        { kind: "file", name: "capture.png", path: "D:\\Inbox\\capture.png" },
      ],
    });
    expect(actions.map((action) => action.id)).toEqual([
      "ihub.context.local-search",
      "ihub.context.ocr",
      "ihub.context.image-tools",
    ]);

    const folderOnly = deriveLauncherContextActions({
      query: "",
      pastedFiles: [{ kind: "folder", name: "album.png", path: "D:\\Inbox\\album.png" }],
    });
    expect(folderOnly.map((action) => action.id)).toEqual([
      "ihub.context.local-search",
      "ihub.context.batch-rename",
    ]);
  });

  it("hides plugin actions until the installed source exposes the exact route", () => {
    const actions = deriveLauncherContextActions({
      query: "",
      hasPastedImage: true,
      pastedImageType: "image/png",
    });
    expect(availableLauncherContextActions(actions, []).map((action) => action.id))
      .toEqual(["ihub.context.screenshot"]);

    const currentOcr = readOfficialPlugin("ihub-plugin-ocr");
    expect(availableLauncherContextActions(actions, [currentOcr]).map((action) => action.id))
      .toEqual(["ihub.context.screenshot", "ihub.context.ocr"]);

    const staleOcr: PluginInfo = {
      ...currentOcr,
      launcherContext: undefined,
      commands: Array.isArray(currentOcr.commands)
        ? currentOcr.commands.filter((command) => command.id !== "recognize-launcher-image")
        : [],
    };
    expect(availableLauncherContextActions(actions, [staleOcr]).map((action) => action.id))
      .toEqual(["ihub.context.screenshot"]);
  });

  it("keeps every derived plugin route backed by its preferred official manifest permission and frontend command", () => {
    const scenarios = [
      deriveLauncherContextActions({
        query: "Please translate this deliberately long sentence into Chinese.",
      }),
      deriveLauncherContextActions({
        query: "",
        hasPastedImage: true,
        pastedImageType: "image/png",
      }),
      deriveLauncherContextActions({
        query: "",
        pastedFiles: [{ kind: "file", name: "capture.png", path: "D:\\Pictures\\capture.png" }],
      }),
    ];
    const routes = new Map<string, Extract<
      ReturnType<typeof deriveLauncherContextActions>[number]["target"],
      { kind: "plugin-handoff" }
    >>();
    for (const action of scenarios.flat()) {
      if (action.target.kind === "plugin-handoff") {
        routes.set(`${action.id}/${action.target.category}`, action.target);
      }
    }

    expect([...routes.keys()].sort()).toEqual([
      "ihub.context.image-tools/files",
      "ihub.context.image-tools/image",
      "ihub.context.ocr/files",
      "ihub.context.ocr/image",
      "ihub.context.text-tools/text",
      "ihub.context.translate/text",
    ]);
    for (const [route, target] of routes) {
      const plugin = readOfficialPlugin(target.preferredPluginId);
      const candidates = eligibleLauncherContextCommands([plugin], {
        id: route,
        suggestedUse: target.suggestedUse,
        categories: [target.category],
        title: route,
        detail: "metadata-only invariant",
      });
      expect(
        candidates.some(({ plugin: candidatePlugin, command }) =>
          candidatePlugin.id === target.preferredPluginId
          && command.id === target.preferredCommandId,
        ),
        `${route} must resolve to ${target.preferredPluginId}/${target.preferredCommandId}`,
      ).toBe(true);
    }
  });
});
