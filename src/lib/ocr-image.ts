export const MAX_OCR_PNG_BYTES = 16 * 1024 * 1024;

export interface OcrImageSize {
  width: number;
  height: number;
}

export function boundedOcrImageSize(
  width: number,
  height: number,
  maximumDimension: number,
): OcrImageSize {
  if (
    !Number.isSafeInteger(width)
    || !Number.isSafeInteger(height)
    || width < 1
    || height < 1
    || !Number.isSafeInteger(maximumDimension)
    || maximumDimension < 1
  ) {
    throw new Error("OCR 图片尺寸无效。");
  }
  const scale = Math.min(1, maximumDimension / Math.max(width, height));
  return {
    width: Math.max(1, Math.round(width * scale)),
    height: Math.max(1, Math.round(height * scale)),
  };
}

function blobDataUrl(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error("无法读取 OCR 图片。"));
    reader.onload = () => {
      if (typeof reader.result === "string" && reader.result.startsWith("data:image/png;base64,")) {
        resolve(reader.result);
      } else {
        reject(new Error("无法生成有效的 OCR PNG。"));
      }
    };
    reader.readAsDataURL(blob);
  });
}

function canvasPng(canvas: HTMLCanvasElement): Promise<Blob> {
  return new Promise((resolve, reject) => {
    canvas.toBlob((blob) => {
      if (blob?.type === "image/png") resolve(blob);
      else reject(new Error("无法生成 OCR PNG。"));
    }, "image/png");
  });
}

export async function prepareOcrPng(
  blob: Blob,
  sourceSize: OcrImageSize,
  maximumDimension: number,
  maximumBytes = MAX_OCR_PNG_BYTES,
): Promise<{ dataUrl: string; resized: boolean; size: OcrImageSize }> {
  if (blob.size < 1 || blob.size > maximumBytes) {
    throw new Error(`OCR 图片必须小于或等于 ${Math.floor(maximumBytes / (1024 * 1024))} MiB。`);
  }
  const size = boundedOcrImageSize(sourceSize.width, sourceSize.height, maximumDimension);
  const resized = size.width !== sourceSize.width || size.height !== sourceSize.height;
  let png = blob;
  if (resized || blob.type !== "image/png") {
    const bitmap = await createImageBitmap(blob);
    try {
      const canvas = document.createElement("canvas");
      canvas.width = size.width;
      canvas.height = size.height;
      const context = canvas.getContext("2d", { alpha: false });
      if (!context) throw new Error("当前 WebView 无法准备 OCR 画布。");
      context.fillStyle = "#ffffff";
      context.fillRect(0, 0, size.width, size.height);
      context.imageSmoothingEnabled = true;
      context.imageSmoothingQuality = "high";
      context.drawImage(bitmap, 0, 0, size.width, size.height);
      png = await canvasPng(canvas);
    } finally {
      bitmap.close();
    }
  }
  if (png.size > maximumBytes) {
    throw new Error(`处理后的 OCR PNG 超过 ${Math.floor(maximumBytes / (1024 * 1024))} MiB。`);
  }
  return { dataUrl: await blobDataUrl(png), resized, size };
}
