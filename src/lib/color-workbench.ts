export interface RgbColor {
  blue: number;
  green: number;
  red: number;
}

export interface HslColor {
  hue: number;
  lightness: number;
  saturation: number;
}

export interface ColorFormat {
  label: string;
  value: string;
}

export interface ColorHarmony {
  colors: string[];
  label: string;
}

const safeHex = /^#[0-9A-F]{6}$/;

export function normalizeColorHex(value: string): string {
  const normalized = value.trim().toUpperCase();
  if (!safeHex.test(normalized)) {
    throw new Error("颜色必须是 #RRGGBB 格式。");
  }
  return normalized;
}

export function colorHexToRgb(value: string): RgbColor {
  const hex = normalizeColorHex(value);
  return {
    red: Number.parseInt(hex.slice(1, 3), 16),
    green: Number.parseInt(hex.slice(3, 5), 16),
    blue: Number.parseInt(hex.slice(5, 7), 16),
  };
}

export function colorRgbToHsl({ red, green, blue }: RgbColor): HslColor {
  const r = red / 255;
  const g = green / 255;
  const b = blue / 255;
  const maximum = Math.max(r, g, b);
  const minimum = Math.min(r, g, b);
  const delta = maximum - minimum;
  const lightness = (maximum + minimum) / 2;
  let hue = 0;
  let saturation = 0;

  if (delta !== 0) {
    saturation = delta / (1 - Math.abs(2 * lightness - 1));
    if (maximum === r) hue = 60 * (((g - b) / delta) % 6);
    else if (maximum === g) hue = 60 * ((b - r) / delta + 2);
    else hue = 60 * ((r - g) / delta + 4);
  }

  return {
    hue: (hue + 360) % 360,
    saturation: saturation * 100,
    lightness: lightness * 100,
  };
}

export function colorHslToHex({ hue, saturation, lightness }: HslColor): string {
  const normalizedHue = ((hue % 360) + 360) % 360;
  const s = Math.min(100, Math.max(0, saturation)) / 100;
  const l = Math.min(100, Math.max(0, lightness)) / 100;
  const chroma = (1 - Math.abs(2 * l - 1)) * s;
  const segment = normalizedHue / 60;
  const intermediate = chroma * (1 - Math.abs((segment % 2) - 1));
  const [rPart, gPart, bPart] = segment < 1
    ? [chroma, intermediate, 0]
    : segment < 2
      ? [intermediate, chroma, 0]
      : segment < 3
        ? [0, chroma, intermediate]
        : segment < 4
          ? [0, intermediate, chroma]
          : segment < 5
            ? [intermediate, 0, chroma]
            : [chroma, 0, intermediate];
  const match = l - chroma / 2;
  return `#${[rPart, gPart, bPart]
    .map((channel) => Math.round((channel + match) * 255).toString(16).padStart(2, "0"))
    .join("")
    .toUpperCase()}`;
}

function rgbToHsv({ red, green, blue }: RgbColor) {
  const r = red / 255;
  const g = green / 255;
  const b = blue / 255;
  const maximum = Math.max(r, g, b);
  const minimum = Math.min(r, g, b);
  const delta = maximum - minimum;
  let hue = 0;
  if (delta !== 0) {
    if (maximum === r) hue = 60 * (((g - b) / delta) % 6);
    else if (maximum === g) hue = 60 * ((b - r) / delta + 2);
    else hue = 60 * ((r - g) / delta + 4);
  }
  return {
    hue: Math.round((hue + 360) % 360),
    saturation: Math.round((maximum === 0 ? 0 : delta / maximum) * 100),
    value: Math.round(maximum * 100),
  };
}

function rgbToCmyk({ red, green, blue }: RgbColor) {
  const r = red / 255;
  const g = green / 255;
  const b = blue / 255;
  const black = 1 - Math.max(r, g, b);
  if (black === 1) return { cyan: 0, magenta: 0, yellow: 0, black: 100 };
  return {
    cyan: Math.round(((1 - r - black) / (1 - black)) * 100),
    magenta: Math.round(((1 - g - black) / (1 - black)) * 100),
    yellow: Math.round(((1 - b - black) / (1 - black)) * 100),
    black: Math.round(black * 100),
  };
}

function srgbToLinear(channel: number): number {
  const value = channel / 255;
  return value <= 0.04045 ? value / 12.92 : ((value + 0.055) / 1.055) ** 2.4;
}

function rgbToLab(rgb: RgbColor) {
  const r = srgbToLinear(rgb.red);
  const g = srgbToLinear(rgb.green);
  const b = srgbToLinear(rgb.blue);
  const x = (0.4124564 * r + 0.3575761 * g + 0.1804375 * b) / 0.95047;
  const y = (0.2126729 * r + 0.7151522 * g + 0.072175 * b) / 1;
  const z = (0.0193339 * r + 0.119192 * g + 0.9503041 * b) / 1.08883;
  const convert = (value: number) => value > 0.008856
    ? Math.cbrt(value)
    : 7.787 * value + 16 / 116;
  const fx = convert(x);
  const fy = convert(y);
  const fz = convert(z);
  return {
    lightness: 116 * fy - 16,
    a: 500 * (fx - fy),
    b: 200 * (fy - fz),
  };
}

function rgbToOklch(rgb: RgbColor) {
  const r = srgbToLinear(rgb.red);
  const g = srgbToLinear(rgb.green);
  const b = srgbToLinear(rgb.blue);
  const l = Math.cbrt(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b);
  const m = Math.cbrt(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b);
  const s = Math.cbrt(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b);
  const lightness = 0.2104542553 * l + 0.793617785 * m - 0.0040720468 * s;
  const a = 1.9779984951 * l - 2.428592205 * m + 0.4505937099 * s;
  const yellowBlue = 0.0259040371 * l + 0.7827717662 * m - 0.808675766 * s;
  const chroma = Math.sqrt(a * a + yellowBlue * yellowBlue);
  const hue = (Math.atan2(yellowBlue, a) * 180 / Math.PI + 360) % 360;
  return { lightness, chroma, hue };
}

export function colorFormats(value: string, alpha = 1): ColorFormat[] {
  const hex = normalizeColorHex(value);
  const rgb = colorHexToRgb(hex);
  const hsl = colorRgbToHsl(rgb);
  const hsv = rgbToHsv(rgb);
  const cmyk = rgbToCmyk(rgb);
  const lab = rgbToLab(rgb);
  const oklch = rgbToOklch(rgb);
  const boundedAlpha = Math.min(1, Math.max(0, alpha));
  return [
    { label: "HEX", value: hex },
    { label: "RGB", value: `${rgb.red}, ${rgb.green}, ${rgb.blue}` },
    { label: "HSV/HSB", value: `${hsv.hue}, ${hsv.saturation}%, ${hsv.value}%` },
    { label: "HSL", value: `${Math.round(hsl.hue)}, ${Math.round(hsl.saturation)}%, ${Math.round(hsl.lightness)}%` },
    { label: "CMYK", value: `${cmyk.cyan}%, ${cmyk.magenta}%, ${cmyk.yellow}%, ${cmyk.black}%` },
    { label: "CIE-LAB", value: `${lab.lightness.toFixed(2)}, ${lab.a.toFixed(2)}, ${lab.b.toFixed(2)}` },
    { label: "OKLCH", value: `${(oklch.lightness * 100).toFixed(2)}% ${oklch.chroma.toFixed(4)} ${oklch.hue.toFixed(1)}` },
    { label: "CSS", value: `rgb(${rgb.red} ${rgb.green} ${rgb.blue} / ${boundedAlpha.toFixed(2)})` },
  ];
}

export function colorHarmonies(value: string): ColorHarmony[] {
  const hsl = colorRgbToHsl(colorHexToRgb(value));
  const shifted = (degrees: number) => colorHslToHex({ ...hsl, hue: hsl.hue + degrees });
  return [
    { label: "互补色", colors: [shifted(180)] },
    { label: "类似色", colors: [shifted(-30), shifted(30)] },
    { label: "分裂互补", colors: [shifted(150), shifted(210)] },
    { label: "三角色", colors: [shifted(120), shifted(240)] },
  ];
}

export function readableTextColor(value: string): "#FFFFFF" | "#000000" {
  const { red, green, blue } = colorHexToRgb(value);
  const luminance = (0.2126 * srgbToLinear(red))
    + (0.7152 * srgbToLinear(green))
    + (0.0722 * srgbToLinear(blue));
  return luminance > 0.179 ? "#000000" : "#FFFFFF";
}
