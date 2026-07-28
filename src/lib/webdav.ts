/**
 * Shared renderer-side policy for the first-party WebDAV browser.
 *
 * The native command repeats this validation before sending a request. Keeping
 * the renderer version small and deterministic lets the UI reject an unsafe
 * endpoint before it ever sends account data across the Tauri boundary.
 */
export const webDavMaxEndpointLength = 2_048;

export interface WebDavDirectoryResponse {
  endpoint: string;
  directory: string;
  xml: string;
}

export interface WebDavDownloadResult {
  bytesWritten: number;
  cancelled: boolean;
  filename: string | null;
}

export interface WebDavUploadResult {
  bytesWritten: number;
  cancelled: boolean;
  filename: string | null;
}

export interface WebDavEntry {
  contentLength: number | null;
  contentType: string | null;
  href: string;
  isCollection: boolean;
  lastModified: string | null;
  name: string;
}

function normalizedHost(hostname: string) {
  return hostname.replace(/^\[|\]$/g, "").replace(/\.$/, "").toLocaleLowerCase();
}

export function isWebDavLoopbackHost(hostname: string) {
  const host = normalizedHost(hostname);
  return host === "localhost" || host === "127.0.0.1" || host === "::1";
}

/**
 * WebDAV Basic credentials must never be sent over a normal network HTTP
 * connection. HTTP remains available only for a local development server.
 */
export function parseWebDavEndpoint(input: string) {
  const value = input.trim();
  if (!value) {
    throw new Error("请输入 WebDAV 地址。");
  }
  if (value.length > webDavMaxEndpointLength) {
    throw new Error(`WebDAV 地址不能超过 ${webDavMaxEndpointLength} 个字符。`);
  }

  let endpoint: URL;
  try {
    endpoint = new URL(value);
  } catch {
    throw new Error("WebDAV 地址不是有效 URL。请包含 https://。");
  }

  if (endpoint.username || endpoint.password) {
    throw new Error("请不要把账号或密码写进 WebDAV 地址；请使用下方单独的字段。");
  }
  if (endpoint.search || endpoint.hash) {
    throw new Error("WebDAV 地址不能包含查询参数或 # 片段。");
  }
  if (endpoint.protocol !== "https:" && endpoint.protocol !== "http:") {
    throw new Error("WebDAV 仅支持 HTTPS；HTTP 只允许本机调试服务。");
  }
  if (endpoint.protocol === "http:" && !isWebDavLoopbackHost(endpoint.hostname)) {
    throw new Error("为保护账号密码，非本机 WebDAV 必须使用 HTTPS。");
  }

  endpoint.pathname = endpoint.pathname.endsWith("/")
    ? endpoint.pathname
    : `${endpoint.pathname}/`;
  return endpoint;
}

function normalizedDirectoryPath(url: URL) {
  return url.pathname.endsWith("/") ? url.pathname : `${url.pathname}/`;
}

export function isWebDavUrlWithinRoot(root: URL, candidate: URL) {
  return root.origin === candidate.origin
    && normalizedDirectoryPath(candidate).startsWith(normalizedDirectoryPath(root));
}

/** Resolves only a same-origin child of the explicit WebDAV root. */
export function resolveWebDavChildUrl(root: URL, directory: URL, href: string) {
  let candidate: URL;
  try {
    candidate = new URL(href, directory);
  } catch {
    return null;
  }
  candidate.search = "";
  candidate.hash = "";
  return isWebDavUrlWithinRoot(root, candidate) ? candidate : null;
}

function firstElementText(element: Element, localName: string) {
  const child = Array.from(element.getElementsByTagName("*"))
    .find((node) => node.localName === localName);
  return child?.textContent?.trim() || null;
}

function isCollectionResponse(element: Element) {
  return Array.from(element.getElementsByTagName("*")).some(
    (node) => node.localName === "collection",
  );
}

function responseHasSuccessfulProperty(element: Element) {
  const statusValues = Array.from(element.getElementsByTagName("*"))
    .filter((node) => node.localName === "status")
    .map((node) => node.textContent ?? "");
  return statusValues.length === 0 || statusValues.some((status) => /\s2\d\d\s/.test(` ${status} `));
}

function readableName(url: URL, isCollection: boolean) {
  const segments = url.pathname.split("/").filter(Boolean);
  const encoded = segments.at(-1);
  if (!encoded) {
    return url.host;
  }
  try {
    return decodeURIComponent(encoded) + (isCollection ? "/" : "");
  } catch {
    return encoded + (isCollection ? "/" : "");
  }
}

function parsedContentLength(value: string | null) {
  if (!value || !/^\d+$/.test(value)) {
    return null;
  }
  const result = Number(value);
  return Number.isSafeInteger(result) ? result : null;
}

/**
 * Parses a bounded XML response received from the native WebDAV command.
 * External hrefs and entries outside the configured root are discarded rather
 * than becoming renderer navigation targets.
 */
export function parseWebDavDirectoryXml(xml: string, root: URL, directory: URL): WebDavEntry[] {
  if (typeof DOMParser === "undefined") {
    throw new Error("当前环境不支持解析 WebDAV 目录响应。");
  }
  const document = new DOMParser().parseFromString(xml, "application/xml");
  if (Array.from(document.getElementsByTagName("*")).some((node) => node.localName === "parsererror")) {
    throw new Error("WebDAV 服务返回的目录数据不是有效 XML。");
  }

  const currentPath = normalizedDirectoryPath(directory);
  const entries = new Map<string, WebDavEntry>();
  const responses = Array.from(document.getElementsByTagName("*"))
    .filter((node) => node.localName === "response");

  for (const response of responses) {
    if (!responseHasSuccessfulProperty(response)) {
      continue;
    }
    const href = firstElementText(response, "href");
    if (!href) {
      continue;
    }
    const url = resolveWebDavChildUrl(root, directory, href);
    if (!url || normalizedDirectoryPath(url) === currentPath) {
      continue;
    }
    const isCollection = isCollectionResponse(response);
    entries.set(url.href, {
      contentLength: parsedContentLength(firstElementText(response, "getcontentlength")),
      contentType: firstElementText(response, "getcontenttype"),
      href: url.href,
      isCollection,
      lastModified: firstElementText(response, "getlastmodified"),
      name: readableName(url, isCollection),
    });
  }

  return [...entries.values()].sort((left, right) => (
    Number(right.isCollection) - Number(left.isCollection)
    || left.name.localeCompare(right.name, "zh-CN", { sensitivity: "base" })
  ));
}

export function formatWebDavBytes(value: number | null) {
  if (value === null) {
    return "大小未知";
  }
  if (value < 1024) {
    return `${value} B`;
  }
  const units = ["KB", "MB", "GB", "TB"];
  let amount = value / 1024;
  let unitIndex = 0;
  while (amount >= 1024 && unitIndex < units.length - 1) {
    amount /= 1024;
    unitIndex += 1;
  }
  return `${amount >= 10 ? amount.toFixed(0) : amount.toFixed(1)} ${units[unitIndex]}`;
}
