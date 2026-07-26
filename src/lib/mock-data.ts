import type { PluginInfo, SearchResult } from "./types";

export const mockResults: SearchResult[] = [
  {
    id: "welcome",
    name: "Welcome to iHub",
    kind: "command",
    score: 1,
    metadata: "Start indexing your local workspace",
    commandId: "ihub.index.default",
  },
  {
    id: "plugin-ocr",
    name: "OCR · extract text from an image",
    kind: "plugin",
    score: 0.98,
    metadata: "Official plugin",
    pluginId: "io.ihub.ocr",
    commandId: "ocr.capture",
  },
  {
    id: "plugin-translate",
    name: "Translate selected text",
    kind: "plugin",
    score: 0.96,
    metadata: "Official plugin",
    pluginId: "io.ihub.translate",
    commandId: "translate.text",
  },
  {
    id: "plugin-color",
    name: "Pick a color from your screen",
    kind: "plugin",
    score: 0.94,
    metadata: "Official plugin",
    pluginId: "io.ihub.colorpick",
    commandId: "colorpick.capture",
  },
];

export const mockPlugins: PluginInfo[] = [
  {
    id: "io.ihub.ocr",
    name: "OCR",
    version: "0.1.0",
    description: "Screenshot, clipboard and image OCR",
    enabled: true,
    hasNativeWorker: true,
    commands: 2,
  },
  {
    id: "io.ihub.translate",
    name: "Translate",
    version: "0.1.0",
    description: "Translate a selection or your clipboard",
    enabled: true,
    hasNativeWorker: false,
    commands: 2,
  },
  {
    id: "io.ihub.colorpick",
    name: "Color Pick",
    version: "0.1.0",
    description: "Global eyedropper and color history",
    enabled: true,
    hasNativeWorker: false,
    commands: 1,
  },
];
