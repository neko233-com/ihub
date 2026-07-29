import type { SearchResult } from "./types";
import browserPreviewApplicationIcon from "../../src-tauri/icons/64x64.png?inline";

/**
 * Browser QA cannot call the native Windows Shell service. This fixture uses
 * iHub's own packaged raster icon and enters through the same strict PNG data
 * URL map as a real `get_system_icons` response, so browser validation covers
 * the renderer boundary without pretending to exercise Shell extraction.
 */
export const browserPreviewSystemIcons: Record<string, string> = {
  "preview-application-ihub": browserPreviewApplicationIcon,
};

export const mockResults: SearchResult[] = [
  {
    id: "preview-application-ihub",
    name: "iHub",
    kind: "application",
    score: 0.99,
    metadata: "应用图标渲染预览 · 原生宿主在桌面端提供",
    path: "C:\\Program Files\\iHub\\ihub.exe",
  },
  {
    id: "welcome",
    name: "Welcome to iHub",
    kind: "command",
    score: 1,
    metadata: "Start indexing your local workspace",
    commandId: "ihub.index.default",
  },
  {
    id: "preview-file-app",
    name: "App.tsx",
    kind: "file",
    score: 0.98,
    metadata: "TypeScript · 当前工作区",
    path: "D:\\Code\\neko233-Projects\\ihub\\src\\App.tsx",
  },
  {
    id: "preview-folder-plugins",
    name: "plugins",
    kind: "folder",
    score: 0.96,
    metadata: "插件 catalog 与官方模板",
    path: "D:\\Code\\neko233-Projects\\ihub\\plugins",
  },
  {
    id: "preview-file-design",
    name: "DESIGN.md",
    kind: "file",
    score: 0.94,
    metadata: "iHub 交互设计说明",
    path: "D:\\Code\\neko233-Projects\\ihub\\docs\\DESIGN.md",
  },
];
