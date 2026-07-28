const maxQrDecodePixels = 16_000_000;
const maxQrDecodeSide = 4_096;
const maxQrDecodeFileBytes = 40 * 1024 * 1024;

export interface QrDecodeDimensions {
  width: number;
  height: number;
}

/**
 * Keep image decoding bounded before pixels enter the renderer. A QR code is
 * still readable after proportional downscaling, while an untrusted giant
 * image cannot allocate an unbounded canvas.
 */
export function qrDecodeDimensions(
  width: number,
  height: number,
  maxPixels = maxQrDecodePixels,
  maxSide = maxQrDecodeSide,
): QrDecodeDimensions {
  if (!Number.isFinite(width) || !Number.isFinite(height) || width <= 0 || height <= 0) {
    throw new Error("图片尺寸无效，无法识别二维码。");
  }
  const safeMaxPixels = Number.isFinite(maxPixels) ? Math.max(1, maxPixels) : maxQrDecodePixels;
  const safeMaxSide = Number.isFinite(maxSide) ? Math.max(1, maxSide) : maxQrDecodeSide;
  const scale = Math.min(
    1,
    safeMaxSide / Math.max(width, height),
    Math.sqrt(safeMaxPixels / (width * height)),
  );
  return {
    width: Math.max(1, Math.floor(width * scale)),
    height: Math.max(1, Math.floor(height * scale)),
  };
}

export async function decodeQrImageFile(file: File): Promise<string | null> {
  if (!file.type.startsWith("image/")) {
    throw new Error("请选择 PNG、JPG、WebP 等图片文件。");
  }
  if (file.size > maxQrDecodeFileBytes) {
    throw new Error("图片超过 40 MB，为保护本机内存未开始识别。");
  }
  if (typeof createImageBitmap !== "function") {
    throw new Error("当前 WebView 不支持本地图片解码，请使用支持 createImageBitmap 的桌面版。");
  }

  const image = await createImageBitmap(file);
  try {
    const dimensions = qrDecodeDimensions(image.width, image.height);
    const canvas = document.createElement("canvas");
    canvas.width = dimensions.width;
    canvas.height = dimensions.height;
    const context = canvas.getContext("2d", { willReadFrequently: true });
    if (!context) {
      throw new Error("当前 WebView 无法读取本地图片像素。");
    }
    context.drawImage(image, 0, 0, dimensions.width, dimensions.height);
    const pixels = context.getImageData(0, 0, dimensions.width, dimensions.height);
    // Load the decoder only after the person chooses an image, keeping the
    // Spotlight launcher path free of this optional parsing code.
    const { default: jsQR } = await import("jsqr");
    return jsQR(pixels.data, dimensions.width, dimensions.height, {
      inversionAttempts: "attemptBoth",
    })?.data ?? null;
  } finally {
    image.close();
  }
}
