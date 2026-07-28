import type { SearchResult } from "./types";

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
