import type { SearchResult } from "./types";

const PNG_DATA_URL_PREFIX = "data:image/png;base64,";
const MAX_MATCHER_IMAGE_BYTES = 4 * 1_024 * 1_024;
const MAX_MATCHER_IMAGE_EDGE = 8_192;
const MAX_MATCHER_IMAGE_PIXELS = 12_000_000;
const MAX_MATCHER_IMAGE_DATA_URL_CHARS = PNG_DATA_URL_PREFIX.length
  + Math.ceil(MAX_MATCHER_IMAGE_BYTES / 3) * 4;

export interface UtoolsContextCommandMatch {
  pluginId: string;
  commandId: string;
  label: string;
  matcherType: "img" | "files" | "window" | string;
  matcherIndex: number;
  mainPush: boolean;
}

function blobDataUrl(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error("无法读取已粘贴图片。"));
    reader.onload = () => typeof reader.result === "string"
      ? resolve(reader.result)
      : reject(new Error("无法编码已粘贴图片。"));
    reader.readAsDataURL(blob);
  });
}

function canvasPng(canvas: HTMLCanvasElement): Promise<Blob> {
  return new Promise((resolve, reject) => {
    canvas.toBlob((blob) => {
      if (blob?.type === "image/png" && blob.size > 0) resolve(blob);
      else reject(new Error("无法把已粘贴图片规范化为 PNG。"));
    }, "image/png");
  });
}

async function drawImageBlob(blob: Blob): Promise<HTMLCanvasElement> {
  const canvas = document.createElement("canvas");
  if (typeof createImageBitmap === "function") {
    const bitmap = await createImageBitmap(blob);
    try {
      canvas.width = bitmap.width;
      canvas.height = bitmap.height;
      canvas.getContext("2d", { alpha: true })?.drawImage(bitmap, 0, 0);
    } finally {
      bitmap.close();
    }
    return canvas;
  }
  const source = URL.createObjectURL(blob);
  try {
    const image = await new Promise<HTMLImageElement>((resolve, reject) => {
      const element = new Image();
      element.onload = () => resolve(element);
      element.onerror = () => reject(new Error("无法解码已粘贴图片。"));
      element.src = source;
    });
    canvas.width = image.naturalWidth;
    canvas.height = image.naturalHeight;
    canvas.getContext("2d", { alpha: true })?.drawImage(image, 0, 0);
    return canvas;
  } finally {
    URL.revokeObjectURL(source);
  }
}

/** Normalizes an explicit launcher paste to the bounded PNG payload expected
 * by the public uTools img matcher event. Rust decodes and validates it again
 * before a visible plugin surface receives anything. */
export async function normalizeUtoolsMatcherImage(blob: Blob): Promise<string> {
  if (!blob.size || !blob.type.toLocaleLowerCase().startsWith("image/")) {
    throw new Error("已粘贴内容不是可读取的图片。");
  }
  let png = blob;
  if (blob.type.toLocaleLowerCase().split(";", 1)[0] !== "image/png") {
    const canvas = await drawImageBlob(blob);
    if (
      !canvas.width
      || !canvas.height
      || canvas.width > MAX_MATCHER_IMAGE_EDGE
      || canvas.height > MAX_MATCHER_IMAGE_EDGE
      || canvas.width * canvas.height > MAX_MATCHER_IMAGE_PIXELS
    ) {
      throw new Error("已粘贴图片尺寸超过 uTools 匹配器安全上限。");
    }
    png = await canvasPng(canvas);
  }
  if (png.size > MAX_MATCHER_IMAGE_BYTES) {
    throw new Error("已粘贴图片规范化后超过 4 MiB，不能交给 uTools 图片匹配器。");
  }
  const dataUrl = await blobDataUrl(png);
  if (!dataUrl.startsWith(PNG_DATA_URL_PREFIX) || dataUrl.length > MAX_MATCHER_IMAGE_DATA_URL_CHARS) {
    throw new Error("已粘贴图片没有形成有效的有界 PNG 数据。");
  }
  return dataUrl;
}

export function utoolsContextMatcherSearchResults(
  matches: readonly UtoolsContextCommandMatch[],
): SearchResult[] {
  return matches.slice(0, 12).map((match, index) => ({
    id: `utools-context:${match.pluginId}:${match.commandId}:${match.matcherIndex}:${index}`,
    name: match.label,
    kind: "plugin",
    score: 970 - index,
    metadata: `${match.matcherType === "img" ? "图片匹配" : match.matcherType === "window" ? "窗口匹配" : "文件匹配"} · uTools 插件`,
    pluginId: match.pluginId,
    commandId: match.commandId,
    utoolsMatcherType: match.matcherType,
    utoolsMatcherIndex: match.matcherIndex,
    utoolsMainPush: match.mainPush,
  }));
}
