export interface CapturePoint {
  x: number;
  y: number;
}

export interface CaptureSize {
  width: number;
  height: number;
}

export interface CaptureRect extends CapturePoint, CaptureSize {}

export interface RegionCaptureSource extends CaptureSize {
  name: string;
  url: string;
  /** Blob URLs are owned by the caller and must be revoked after editing. */
  revokeOnClose?: boolean;
}

export interface CroppedCapture extends CaptureSize {
  blob: Blob;
  name: string;
}

export const MAX_REGION_CAPTURE_EDGE = 8_192;
export const MAX_REGION_CAPTURE_PIXELS = 24_000_000;
export const MIN_REGION_CAPTURE_EDGE = 2;

function finite(value: number): number {
  return Number.isFinite(value) ? value : 0;
}

function clamp(value: number, minimum: number, maximum: number): number {
  return Math.min(maximum, Math.max(minimum, finite(value)));
}

export function validateRegionCaptureSize(size: CaptureSize): CaptureSize {
  const width = Math.round(finite(size.width));
  const height = Math.round(finite(size.height));
  if (width < 1 || height < 1) {
    throw new Error("截图画面没有可选择的像素。");
  }
  if (width > MAX_REGION_CAPTURE_EDGE || height > MAX_REGION_CAPTURE_EDGE) {
    throw new Error(`截图画面的单边尺寸不能超过 ${MAX_REGION_CAPTURE_EDGE}px。`);
  }
  if (width * height > MAX_REGION_CAPTURE_PIXELS) {
    throw new Error("截图画面不能超过 2400 万像素。");
  }
  return { width, height };
}

/**
 * Normalizes any drag direction into a source-pixel rectangle. Both endpoints
 * are clamped before rounding, so a pointer leaving the preview cannot create
 * an oversized crop or negative canvas dimensions.
 */
export function regionFromDrag(
  anchor: CapturePoint,
  pointer: CapturePoint,
  sourceSize: CaptureSize,
): CaptureRect {
  const { width, height } = validateRegionCaptureSize(sourceSize);
  const anchorX = clamp(anchor.x, 0, width);
  const anchorY = clamp(anchor.y, 0, height);
  const pointerX = clamp(pointer.x, 0, width);
  const pointerY = clamp(pointer.y, 0, height);
  const left = Math.floor(Math.min(anchorX, pointerX));
  const top = Math.floor(Math.min(anchorY, pointerY));
  const right = Math.ceil(Math.max(anchorX, pointerX));
  const bottom = Math.ceil(Math.max(anchorY, pointerY));

  return {
    x: left,
    y: top,
    width: Math.max(0, right - left),
    height: Math.max(0, bottom - top),
  };
}

export function isUsableCaptureRegion(region: CaptureRect | null): region is CaptureRect {
  return Boolean(
    region
    && Number.isInteger(region.x)
    && Number.isInteger(region.y)
    && Number.isInteger(region.width)
    && Number.isInteger(region.height)
    && region.width >= MIN_REGION_CAPTURE_EDGE
    && region.height >= MIN_REGION_CAPTURE_EDGE,
  );
}

/**
 * Converts a pointer inside the rendered preview to source-image pixels. The
 * preview uses object-fit: contain; callers pass the actual image content box,
 * not the outer panel, so this mapping remains DPI independent.
 */
export function pointInCaptureSource(
  client: CapturePoint,
  renderedBounds: CaptureRect,
  sourceSize: CaptureSize,
): CapturePoint {
  const { width, height } = validateRegionCaptureSize(sourceSize);
  if (renderedBounds.width <= 0 || renderedBounds.height <= 0) {
    return { x: 0, y: 0 };
  }
  return {
    x: clamp(
      ((client.x - renderedBounds.x) / renderedBounds.width) * width,
      0,
      width,
    ),
    y: clamp(
      ((client.y - renderedBounds.y) / renderedBounds.height) * height,
      0,
      height,
    ),
  };
}

export function captureRegionStyle(
  region: CaptureRect,
  sourceSize: CaptureSize,
): Record<"height" | "left" | "top" | "width", string> {
  const { width, height } = validateRegionCaptureSize(sourceSize);
  return {
    left: `${(region.x / width) * 100}%`,
    top: `${(region.y / height) * 100}%`,
    width: `${(region.width / width) * 100}%`,
    height: `${(region.height / height) * 100}%`,
  };
}

function loadCaptureImage(url: string): Promise<HTMLImageElement> {
  return new Promise((resolve, reject) => {
    const image = new Image();
    image.decoding = "async";
    image.onload = () => resolve(image);
    image.onerror = () => reject(new Error("无法读取截图画面。"));
    image.src = url;
  });
}

function canvasPngBlob(canvas: HTMLCanvasElement): Promise<Blob> {
  return new Promise((resolve, reject) => {
    canvas.toBlob((blob) => {
      if (blob?.type === "image/png") {
        resolve(blob);
      } else {
        reject(new Error("浏览器没有生成有效的 PNG 选区。"));
      }
    }, "image/png");
  });
}

export async function cropCaptureRegion(
  source: RegionCaptureSource,
  region: CaptureRect,
): Promise<CroppedCapture> {
  const size = validateRegionCaptureSize(source);
  if (
    !isUsableCaptureRegion(region)
    || region.x < 0
    || region.y < 0
    || region.x + region.width > size.width
    || region.y + region.height > size.height
  ) {
    throw new Error("请拖拽选择至少 2 × 2 像素的有效区域。");
  }

  const image = await loadCaptureImage(source.url);
  const canvas = document.createElement("canvas");
  canvas.width = region.width;
  canvas.height = region.height;
  const context = canvas.getContext("2d", { alpha: false });
  if (!context) {
    throw new Error("当前 WebView 无法创建截图裁剪画布。");
  }
  context.drawImage(
    image,
    region.x,
    region.y,
    region.width,
    region.height,
    0,
    0,
    region.width,
    region.height,
  );

  return {
    blob: await canvasPngBlob(canvas),
    name: source.name.replace(/(?:\.[^.]+)?$/, "-region.png"),
    width: region.width,
    height: region.height,
  };
}

/**
 * A deterministic, local-only SVG frame used by browser development QA. It
 * contains no external resources and is omitted from production UI.
 */
export function createRegionCaptureDemoSource(): RegionCaptureSource {
  const width = 960;
  const height = 540;
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="${width}" height="${height}" viewBox="0 0 ${width} ${height}">
    <rect width="960" height="540" fill="#eef0f2"/>
    <rect x="36" y="34" width="888" height="54" rx="12" fill="#fff"/>
    <circle cx="68" cy="61" r="10" fill="#ff6b6b"/>
    <circle cx="98" cy="61" r="10" fill="#f5c451"/>
    <circle cx="128" cy="61" r="10" fill="#4ecb71"/>
    <rect x="36" y="116" width="240" height="388" rx="18" fill="#dfe4e8"/>
    <rect x="306" y="116" width="618" height="228" rx="18" fill="#3f51b5"/>
    <rect x="338" y="150" width="286" height="22" rx="11" fill="#fff" opacity=".94"/>
    <rect x="338" y="190" width="424" height="14" rx="7" fill="#fff" opacity=".62"/>
    <rect x="306" y="374" width="294" height="130" rx="18" fill="#fff"/>
    <rect x="630" y="374" width="294" height="130" rx="18" fill="#fff"/>
    <text x="338" y="292" fill="#fff" font-family="system-ui,sans-serif" font-size="34" font-weight="700">iHub region QA</text>
  </svg>`;
  return {
    width,
    height,
    name: "ihub-region-demo.png",
    url: `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`,
  };
}
