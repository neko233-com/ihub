/**
 * A small, client-side view of the official catalog.
 *
 * The registry remains the source of truth for releases. This list deliberately
 * marks bootstrap entries as such so the UI never presents an unpublished
 * repository as a downloadable plugin.
 */
export type PluginCatalogCategory =
  | "productivity"
  | "media"
  | "text"
  | "developer"
  | "system";

export type PluginCatalogDistribution = "installable" | "bootstrap" | "builtin";

/** Target spellings shared by the official registry and the native host. */
export type PluginCatalogTarget =
  | "windows-x86_64"
  | "windows-aarch64"
  | "darwin-x86_64"
  | "darwin-aarch64";

export type BuiltinToolId =
  | "search"
  | "color"
  | "screenshot"
  | "clipboard"
  | "json"
  | "markdown"
  | "note"
  | "convert"
  | "calculator"
  | "time"
  | "qrcode"
  | "cloud"
  | "record"
  | "rename"
  | "developer";

export type PluginCatalogIcon =
  | "search"
  | "ocr"
  | "translate"
  | "palette"
  | "clipboard"
  | "screenshot"
  | "json"
  | "video"
  | "rename"
  | "code"
  | "qrcode"
  | "cloud"
  | "window"
  | "note"
  | "converter";

export interface PluginCatalogEntry {
  id: string;
  name: string;
  description: string;
  category: PluginCatalogCategory;
  distribution: PluginCatalogDistribution;
  /** A GitHub shorthand understood by the desktop installer. */
  source?: string;
  aliases?: string[];
  tags: string[];
  icon: PluginCatalogIcon;
  builtinTool?: BuiltinToolId;
  featured?: boolean;
  native?: boolean;
  /**
   * Marks a first-party project that may be explicitly linked only when the
   * native host validates it beside the current source checkout. Installable
   * entries still fall back to their immutable Git release when no trusted
   * development checkout is available.
   */
  workspaceProject?: boolean;
  /** Omit for cross-platform plugins; only installable official entries are gated. */
  supportedTargets?: ReadonlyArray<PluginCatalogTarget>;
}

export interface InstalledRailPlugin {
  id: string;
  name: string;
  description?: string;
}

export interface InstalledRailEntry<TInstalled extends InstalledRailPlugin = InstalledRailPlugin> {
  entry: PluginCatalogEntry;
  installed?: TInstalled;
}

export const pluginCatalogCategories: ReadonlyArray<{
  id: PluginCatalogCategory;
  label: string;
}> = [
  { id: "productivity", label: "效率" },
  { id: "text", label: "文本与数据" },
  { id: "media", label: "图像与媒体" },
  { id: "developer", label: "开发者" },
  { id: "system", label: "系统" },
];

export const pluginCatalog: ReadonlyArray<PluginCatalogEntry> = [
  {
    id: "ihub-local-search",
    name: "本地搜索",
    description: "从主命令框即时查找已建立索引的文件与文件夹。",
    category: "productivity",
    distribution: "builtin",
    tags: ["Everything", "文件", "索引", "搜索"],
    icon: "search",
    builtinTool: "search",
    featured: true,
  },
  {
    id: "ihub-color-picker",
    name: "取色器",
    description: "在本机转换 HEX、RGB、HSL 与 CSS；桌面端还可在明确倒计时后从当前光标位置拾取一次颜色。",
    category: "media",
    distribution: "builtin",
    tags: ["取色", "颜色", "HEX", "RGB", "HSL", "CSS", "离线"],
    icon: "palette",
    builtinTool: "color",
  },
  {
    id: "ihub-plugin-ocr",
    name: "OCR 文字识别",
    description: "Windows x64 离线 OCR；启动器只交接图片/文件元数据，仍需重新选择文件后才会启动原生 worker。",
    category: "media",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-ocr@v0.2.1",
    aliases: ["io.ihub.ocr"],
    tags: ["OCR", "截图", "文字识别", "图片"],
    icon: "ocr",
    native: true,
    featured: true,
    supportedTargets: ["windows-x86_64"],
  },
  {
    id: "ihub-plugin-translate",
    name: "翻译（插件）",
    description: "显式交接的文本只会预填；仅在点击后才向你填写的 LibreTranslate 兼容 HTTPS 服务发送。",
    category: "text",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-translate@v1.1.0",
    aliases: ["io.ihub.translate"],
    tags: ["翻译", "LibreTranslate", "HTTPS", "会话密钥"],
    icon: "translate",
  },
  {
    id: "ihub-plugin-colorpick",
    name: "取色器（插件）",
    description: "离线转换 HEX、RGB、HSL 与 CSS；每次系统像素取色都要点击并经 iHub 单独确认。",
    category: "media",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-colorpick@v1.1.0",
    aliases: ["io.ihub.colorpick"],
    tags: ["颜色", "HEX", "RGB", "HSL", "离线"],
    icon: "palette",
    featured: true,
  },
  {
    id: "ihub-json-tools",
    name: "JSON 工具",
    description: "离线校验、格式化并复制 JSON；内容不会上传。",
    category: "text",
    distribution: "builtin",
    tags: ["JSON", "格式化", "校验", "开发"],
    icon: "json",
    builtinTool: "json",
  },
  {
    id: "ihub-markdown-workbench",
    name: "Markdown 工作台",
    description: "离线 Markdown 写作、安全预览、文档目录与本地导入导出；文本不会作为 HTML 执行或上传。",
    category: "text",
    distribution: "builtin",
    tags: ["Markdown", "README", "笔记", "预览", "离线", "安全"],
    icon: "note",
    builtinTool: "markdown",
    featured: true,
  },
  {
    id: "ihub-plugin-json-tools",
    name: "JSON 工具（插件示例）",
    description: "可导入的官方 TypeScript iframe 插件：离线格式化、压缩、校验与复制 JSON。",
    category: "text",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-json-tools@v1.0.1",
    tags: ["JSON", "TypeScript", "插件示例", "离线"],
    icon: "json",
  },
  {
    id: "ihub-plugin-text-tools",
    name: "文本工具（插件）",
    description: "可导入的官方 TypeScript 插件：离线 UUID、SHA-256、Base64、URL、命名转换与空白清理。",
    category: "text",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-text-tools@v1.1.0",
    tags: ["文本", "UUID", "SHA-256", "Base64", "URL", "离线"],
    icon: "converter",
  },
  {
    id: "ihub-screen-recorder",
    name: "屏幕录制",
    description: "选择屏幕、窗口或标签页，完成后导出 WebM 录制。",
    category: "media",
    distribution: "builtin",
    tags: ["录屏", "视频", "WebM", "屏幕"],
    icon: "video",
    builtinTool: "record",
  },
  {
    id: "ihub-batch-rename",
    name: "批量重命名",
    description: "先预览再确认执行，支持文本替换和正则表达式。",
    category: "productivity",
    distribution: "builtin",
    tags: ["文件", "重命名", "正则", "批处理"],
    icon: "rename",
    builtinTool: "rename",
  },
  {
    id: "ihub-developer-tools",
    name: "插件开发者工具",
    description: "创建 TypeScript 前端 + Rust JSONL worker 模板，并从本机开始调试插件。",
    category: "developer",
    distribution: "builtin",
    tags: ["模板", "TypeScript", "Vite", "Rust", "JSONL"],
    icon: "code",
    builtinTool: "developer",
    featured: true,
  },
  {
    id: "ihub-plugin-developer-tools",
    name: "插件开发者工具（官方插件）",
    description: "可导入的官方 TypeScript 插件：系统选择父文件夹后，以短期授权创建独立的 Vite + Rust worker 模板。",
    category: "developer",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-developer-tools@v1.0.1",
    tags: ["模板", "TypeScript", "Vite", "Rust", "授权目录"],
    icon: "code",
    featured: true,
  },
  {
    id: "ihub-clipboard-history",
    name: "剪贴板历史",
    description: "在你明确开启后，本机保存、固定和复用纯文本剪贴板记录。",
    category: "productivity",
    distribution: "builtin",
    tags: ["剪贴板", "历史", "固定"],
    icon: "clipboard",
    builtinTool: "clipboard",
  },
  {
    id: "ihub-plugin-clipboard",
    name: "剪贴板历史（插件）",
    description: "可导入的官方 TypeScript 插件：仅在你点击收集时读取当前纯文本；可另行点击加载受权限保护、只读的 iHub 历史快照。",
    category: "productivity",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-clipboard@v1.0.1",
    tags: ["剪贴板", "历史", "固定", "本地", "隐私", "只读"],
    icon: "clipboard",
    featured: true,
  },
  {
    id: "ihub-plugin-screenshot",
    name: "截图（插件）",
    description: "点击后才请求浏览器屏幕、窗口或标签页共享；从所选内容截取一帧本地 PNG，不提供原生截图或全局热键。",
    category: "media",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-screenshot@v1.0.1",
    tags: ["截图", "PNG", "getDisplayMedia", "浏览器"],
    icon: "screenshot",
  },
  {
    id: "ihub-screenshot",
    name: "截图",
    description: "读取显示器、窗口或标签页的一帧，再拖拽矩形选区裁剪 PNG；取消即丢弃，文件只会在你下载时保存。",
    category: "media",
    distribution: "builtin",
    tags: ["截图", "PNG", "屏幕", "窗口", "本地"],
    icon: "screenshot",
    builtinTool: "screenshot",
  },
  {
    id: "ihub-plugin-image-tools",
    name: "图片工具（插件）",
    description: "离线批量缩放、转换 PNG/JPEG/WebP；仅处理你选择或拖入页面的图片，并可本地导出 ZIP。",
    category: "media",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-image-tools@v1.1.0",
    tags: ["图片", "缩放", "转换", "PNG", "JPEG", "WebP", "离线", "ZIP"],
    icon: "screenshot",
    featured: true,
  },
  {
    id: "ihub-plugin-base-converter",
    name: "进制转换",
    description: "可导入的官方 TypeScript 插件：BigInt 二、八、十、十六进制与 UTF-8 / Base64 本地转换。",
    category: "developer",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-base-converter@v1.0.0",
    tags: ["进制", "编码", "BigInt", "Base64", "离线"],
    icon: "converter",
    featured: true,
  },
  {
    id: "ihub-converter",
    name: "进制与文本转换",
    description: "离线完成 BigInt 二、八、十、十六进制，以及 UTF-8 Hex 与 Base64 的双向转换。",
    category: "developer",
    distribution: "builtin",
    tags: ["进制", "编码", "BigInt", "Base64", "Hex", "离线"],
    icon: "converter",
    builtinTool: "convert",
  },
  {
    id: "ihub-calculator",
    name: "计算器",
    description: "离线计算四则、括号、百分号、幂与小数表达式；结果可复制并保留本机历史。",
    category: "productivity",
    distribution: "builtin",
    tags: ["计算器", "数学", "表达式", "离线", "Spotlight"],
    icon: "converter",
    builtinTool: "calculator",
    featured: true,
  },
  {
    id: "ihub-time-tools",
    name: "时间与时间戳",
    description: "离线转换 Unix 秒、毫秒与日期文本，并按本机、UTC 或指定 IANA 时区显示。",
    category: "developer",
    distribution: "builtin",
    tags: ["时间戳", "timestamp", "Unix", "Epoch", "日期", "ISO 8601", "UTC", "IANA", "时区", "10 位", "13 位"],
    icon: "converter",
    builtinTool: "time",
    featured: true,
  },
  {
    id: "ihub-plugin-quick-note",
    name: "速记（插件）",
    description: "可导入的官方 TypeScript 插件：在插件隔离本机设置中记录、固定、搜索、复制和删除便签。",
    category: "productivity",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-quick-note@v1.0.0",
    tags: ["笔记", "速记", "搜索", "本地", "离线"],
    icon: "note",
    featured: true,
  },
  {
    id: "ihub-quick-note",
    name: "快速便签",
    description: "在本机保存、搜索、复制和删除临时便签；不会上传或同步内容。",
    category: "productivity",
    distribution: "builtin",
    tags: ["便签", "速记", "笔记", "搜索", "本地", "离线"],
    icon: "note",
    builtinTool: "note",
  },
  {
    id: "ihub-plugin-screen-record",
    name: "录屏（WebM 插件）",
    description: "点击后才请求系统共享；在当前 WebView 本地录制、预览并下载 WebM，单次最多 30 分钟或 512 MiB。",
    category: "media",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-screen-record@v1.0.3",
    tags: ["录屏", "WebM", "本地", "视频"],
    icon: "video",
  },
  {
    id: "ihub-plugin-batch-rename",
    name: "批量重命名（插件）",
    description: "可导入的官方 TypeScript 插件：支持字面量、正则与 {n} 编号，且只对系统选择器授权的目录生成原生预览。",
    category: "productivity",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-batch-rename@v1.1.0",
    tags: ["重命名", "编号", "文件", "正则", "预览", "本地"],
    icon: "rename",
    featured: true,
  },
  {
    id: "ihub-qrcode",
    name: "二维码生成与识别",
    description: "离线生成文本或 URL 二维码，也可识别你主动选择的本地图片；内容不会上传。",
    category: "media",
    distribution: "builtin",
    tags: ["二维码", "生成", "识别", "扫码", "图片", "PNG", "离线"],
    icon: "qrcode",
    builtinTool: "qrcode",
  },
  {
    id: "ihub-cloud-drive",
    name: "云盘（WebDAV）",
    description: "第一方受限 WebDAV 连接器：点击后才连接，拒绝重定向与非本机 HTTP；账号和密码不写入磁盘。",
    category: "productivity",
    distribution: "builtin",
    tags: ["云盘", "WebDAV", "NAS", "文件", "目录", "本地凭据", "安全"],
    icon: "cloud",
    builtinTool: "cloud",
  },
  {
    id: "ihub-plugin-pdf-tools",
    name: "PDF 工具（插件）",
    description: "在当前 WebView 内合并、拆分、重排、删除和旋转你明确选择的 PDF；全程本机处理。",
    category: "productivity",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-pdf-tools@v0.1.1",
    tags: ["PDF", "合并", "拆分", "旋转", "页面", "离线"],
    icon: "note",
  },
  {
    id: "ihub-plugin-archive-tools",
    name: "ZIP 压缩与解压（插件）",
    description: "在当前 WebView 内创建 ZIP、检查归档并逐项导出；带压缩炸弹、ZIP64 与路径穿越预检。",
    category: "productivity",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-archive-tools@v0.1.0",
    tags: ["ZIP", "压缩", "解压", "归档", "文件", "离线"],
    icon: "rename",
  },
  {
    id: "ihub-plugin-web-actions",
    name: "网页动作（插件）",
    description: "本机规范化网址或搜索词，预览并明确点击后才交给默认浏览器；只允许 HTTP(S)。",
    category: "productivity",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-web-actions@v0.1.0",
    tags: ["网页", "网址", "浏览器", "搜索", "HTTP", "HTTPS"],
    icon: "window",
  },
  {
    id: "ihub-plugin-qrcode",
    name: "二维码生成（插件）",
    description: "离线将文本或 URL 生成二维码，并在当前页面预览与下载 PNG。",
    category: "media",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-qrcode@v1.0.0",
    tags: ["二维码", "生成", "PNG", "离线"],
    icon: "qrcode",
  },
  {
    id: "ihub-plugin-window-manager",
    name: "启动器窗口布局",
    description: "只对 iHub 主启动器执行居中、左右贴靠和切换置顶；不控制其他应用窗口。",
    category: "system",
    distribution: "installable",
    workspaceProject: true,
    source: "neko233-com/ihub-plugin-window-manager@v1.0.2",
    tags: ["启动器", "窗口", "贴靠", "置顶", "本地"],
    icon: "window",
  },
];

export function findCatalogEntry(pluginId: string) {
  return pluginCatalog.find(
    (entry) => entry.id === pluginId || entry.aliases?.includes(pluginId),
  );
}

export type PluginCatalogAcquisition =
  | "builtin"
  | "workspace"
  | "remote"
  | "pending";

/**
 * Chooses the safest usable source for a catalog entry. A trusted checkout is
 * preferred on a development machine so saved plugin builds are picked up in
 * place; normal installations retain the immutable Git release path.
 */
export function preferredPluginAcquisition(
  entry: PluginCatalogEntry,
  workspaceAvailable: boolean,
): PluginCatalogAcquisition {
  if (entry.distribution === "builtin") {
    return "builtin";
  }
  if (entry.workspaceProject && workspaceAvailable) {
    return "workspace";
  }
  if (entry.distribution === "installable" && entry.source) {
    return "remote";
  }
  return "pending";
}

function installedRailIdentity(value: string) {
  return value.trim().toLocaleLowerCase();
}

/**
 * Builds the launcher's app-like left rail without mixing marketplace
 * navigation into it. Host order is preserved for installed plugins, catalog
 * order is preserved for built-ins, aliases collapse onto their canonical
 * catalog entry, and disabled plugins remain visible for management.
 */
export function buildInstalledRailEntries<TInstalled extends InstalledRailPlugin>(
  installedPlugins: ReadonlyArray<TInstalled>,
  catalog: ReadonlyArray<PluginCatalogEntry> = pluginCatalog,
): InstalledRailEntry<TInstalled>[] {
  const seen = new Set<string>();
  const entries: InstalledRailEntry<TInstalled>[] = [];

  for (const installed of installedPlugins) {
    const catalogEntry = catalog.find(
      (entry) => entry.id === installed.id || entry.aliases?.includes(installed.id),
    );
    const entry = catalogEntry ?? {
      id: installed.id,
      name: installed.name,
      description: installed.description ?? "从 GitHub 导入的插件。",
      category: "productivity",
      distribution: "bootstrap",
      tags: [installed.id, "已安装"],
      icon: "note",
    } satisfies PluginCatalogEntry;
    const identity = installedRailIdentity(entry.id);
    if (seen.has(identity)) {
      continue;
    }
    seen.add(identity);
    entries.push({ entry, installed });
  }

  for (const entry of catalog) {
    if (entry.distribution !== "builtin") {
      continue;
    }
    const identity = installedRailIdentity(entry.id);
    if (seen.has(identity)) {
      continue;
    }
    seen.add(identity);
    entries.push({ entry });
  }

  return entries;
}
