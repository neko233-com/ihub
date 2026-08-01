import {
  Check,
  Copy,
  Grid2X2,
  Heart,
  Palette,
  Pipette,
  Plus,
  Star,
  Trash2,
  X,
} from "lucide-react";
import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
} from "react";
import {
  colorFormats,
  colorHarmonies,
  colorHexToRgb,
  colorHslToHex,
  colorRgbToHsl,
  normalizeColorHex,
  readableTextColor,
} from "../lib/color-workbench";
import { LiveColorPicker } from "./LiveColorPicker";

interface ColorWorkbenchProps {
  color: string;
  onClose: () => void;
  onColorChange: (color: string) => void;
  onCopy: (value: string, label: string) => Promise<void> | void;
  onPickScreenColor: () => Promise<void> | void;
  onStartWindowDrag?: () => void;
  onToast: (message: string) => void;
}

type ColorView = "editor" | "palettes" | "favorites";

const favoriteStorageKey = "ihub.color-workbench.favorites.v1";
const maximumFavorites = 24;
const applePalette = [
  { color: "#0A84FF", name: "系统蓝" },
  { color: "#5E5CE6", name: "系统靛" },
  { color: "#BF5AF2", name: "系统紫" },
  { color: "#FF375F", name: "系统粉" },
  { color: "#FF9F0A", name: "系统橙" },
  { color: "#30D158", name: "薄荷绿" },
  { color: "#64D2FF", name: "系统青" },
  { color: "#FFD60A", name: "系统黄" },
];

function restoreFavorites(): string[] {
  try {
    const value = JSON.parse(window.localStorage.getItem(favoriteStorageKey) ?? "[]");
    if (!Array.isArray(value)) return [];
    return value
      .filter((entry): entry is string => typeof entry === "string")
      .flatMap((entry) => {
        try {
          return [normalizeColorHex(entry)];
        } catch {
          return [];
        }
      })
      .filter((entry, index, all) => all.indexOf(entry) === index)
      .slice(0, maximumFavorites);
  } catch {
    return [];
  }
}

export function ColorWorkbench({
  color,
  onClose,
  onColorChange,
  onCopy,
  onPickScreenColor,
  onStartWindowDrag,
  onToast,
}: ColorWorkbenchProps) {
  const normalizedColor = normalizeColorHex(color);
  const hsl = useMemo(
    () => colorRgbToHsl(colorHexToRgb(normalizedColor)),
    [normalizedColor],
  );
  const [alpha, setAlpha] = useState(1);
  const [view, setView] = useState<ColorView>("editor");
  const [favorites, setFavorites] = useState<string[]>(restoreFavorites);
  const [showLivePicker, setShowLivePicker] = useState(false);
  const wheelRef = useRef<HTMLDivElement | null>(null);
  const formats = useMemo(
    () => colorFormats(normalizedColor, alpha),
    [alpha, normalizedColor],
  );
  const harmonies = useMemo(() => colorHarmonies(normalizedColor), [normalizedColor]);

  useEffect(() => {
    window.localStorage.setItem(favoriteStorageKey, JSON.stringify(favorites));
  }, [favorites]);

  const updateWheel = (event: ReactPointerEvent<HTMLDivElement>) => {
    const wheel = wheelRef.current;
    if (!wheel) return;
    const bounds = wheel.getBoundingClientRect();
    const radius = Math.min(bounds.width, bounds.height) / 2;
    const x = event.clientX - bounds.left - bounds.width / 2;
    const y = event.clientY - bounds.top - bounds.height / 2;
    const distance = Math.min(radius, Math.sqrt(x * x + y * y));
    const hue = (Math.atan2(y, x) * 180 / Math.PI + 360) % 360;
    onColorChange(colorHslToHex({
      hue,
      saturation: (distance / radius) * 100,
      lightness: hsl.lightness,
    }));
  };

  const chooseColor = (nextColor: string) => {
    onColorChange(normalizeColorHex(nextColor));
    setView("editor");
  };

  const toggleFavorite = () => {
    setFavorites((current) => current.includes(normalizedColor)
      ? current.filter((entry) => entry !== normalizedColor)
      : [normalizedColor, ...current].slice(0, maximumFavorites));
  };

  const wheelDistance = hsl.saturation / 2;
  const wheelAngle = hsl.hue * Math.PI / 180;

  return (
    <section aria-label="颜色工作台" className="color-workbench">
      <header
        className="color-workbench__header"
        data-tauri-drag-region="true"
        onMouseDown={(event) => {
          if (event.button === 0 && event.target === event.currentTarget) onStartWindowDrag?.();
        }}
      >
        <div className="color-workbench__identity">
          <span className="color-workbench__app-icon" aria-hidden="true"><Palette size={18} /></span>
          <strong id="color-workbench-title">颜色助手</strong>
          <span>Color</span>
        </div>
        <div className="color-workbench__header-actions">
          <button onClick={toggleFavorite} type="button">
            <Star fill={favorites.includes(normalizedColor) ? "currentColor" : "none"} size={15} />
            {favorites.includes(normalizedColor) ? "已收藏" : "收藏"}
          </button>
          <button aria-label="关闭颜色工作台" onClick={onClose} type="button"><X size={17} /></button>
        </div>
      </header>

      <div className="color-workbench__body">
        <nav aria-label="颜色工具" className="color-workbench__rail">
          <button className={view === "editor" ? "is-active" : ""} onClick={() => setView("editor")} type="button">
            <Palette size={17} />颜色
          </button>
          <button className={view === "palettes" ? "is-active" : ""} onClick={() => setView("palettes")} type="button">
            <Grid2X2 size={17} />Apple 色卡
          </button>
          <button className={view === "favorites" ? "is-active" : ""} onClick={() => setView("favorites")} type="button">
            <Heart size={17} />收藏颜色
            <span>{favorites.length}</span>
          </button>
          <div className="color-workbench__rail-spacer" />
          <label className="color-workbench__native-color">
            <Pipette size={15} />系统面板
            <input
              aria-label="使用系统颜色面板"
              onChange={(event) => chooseColor(event.target.value)}
              type="color"
              value={normalizedColor}
            />
          </label>
        </nav>

        {view === "editor" ? (
          <>
            <main className="color-workbench__picker">
              <div className="color-workbench__picker-topline">
                <button
                  className="color-workbench__eyedropper"
                  onClick={() => setShowLivePicker((current) => !current)}
                  type="button"
                >
                  <Pipette size={18} />屏幕取色
                </button>
                <button className="color-workbench__web-eyedropper" onClick={() => void onPickScreenColor()} type="button">
                  WebView 吸管
                </button>
              </div>
              <div
                aria-label="色相与饱和度色轮"
                aria-valuemax={360}
                aria-valuemin={0}
                aria-valuenow={Math.round(hsl.hue)}
                aria-valuetext={`色相 ${Math.round(hsl.hue)} 度，饱和度 ${Math.round(hsl.saturation)}%`}
                className="color-workbench__wheel"
                onKeyDown={(event) => {
                  const changes: Partial<{ hue: number; saturation: number }> = {};
                  if (event.key === "ArrowLeft") changes.hue = hsl.hue - 1;
                  else if (event.key === "ArrowRight") changes.hue = hsl.hue + 1;
                  else if (event.key === "ArrowDown") changes.saturation = hsl.saturation - 1;
                  else if (event.key === "ArrowUp") changes.saturation = hsl.saturation + 1;
                  else return;
                  event.preventDefault();
                  onColorChange(colorHslToHex({ ...hsl, ...changes }));
                }}
                onPointerDown={(event) => {
                  event.currentTarget.setPointerCapture(event.pointerId);
                  updateWheel(event);
                }}
                onPointerMove={(event) => {
                  if (event.currentTarget.hasPointerCapture(event.pointerId)) updateWheel(event);
                }}
                ref={wheelRef}
                role="slider"
                tabIndex={0}
              >
                <span
                  className="color-workbench__wheel-handle"
                  style={{
                    backgroundColor: normalizedColor,
                    left: `${50 + Math.cos(wheelAngle) * wheelDistance}%`,
                    top: `${50 + Math.sin(wheelAngle) * wheelDistance}%`,
                  }}
                />
              </div>
              <label className="color-workbench__slider">
                <span>明度</span>
                <input
                  aria-label="颜色明度"
                  max="100"
                  min="0"
                  onChange={(event) => onColorChange(colorHslToHex({ ...hsl, lightness: Number(event.target.value) }))}
                  style={{ "--slider-color": colorHslToHex({ ...hsl, lightness: 50 }) } as CSSProperties}
                  type="range"
                  value={Math.round(hsl.lightness)}
                />
                <strong>{Math.round(hsl.lightness)}%</strong>
              </label>
              <label className="color-workbench__slider color-workbench__slider--alpha">
                <span>透明</span>
                <input aria-label="颜色透明度" max="100" min="0" onChange={(event) => setAlpha(Number(event.target.value) / 100)} type="range" value={Math.round(alpha * 100)} />
                <strong>{Math.round(alpha * 100)}%</strong>
              </label>
              {showLivePicker ? (
                <div className="color-workbench__live-panel">
                  <LiveColorPicker
                    onConfirm={(sample) => {
                      chooseColor(sample.hex);
                      setShowLivePicker(false);
                      void onCopy(sample.hex, "HEX");
                    }}
                    onStatus={onToast}
                  />
                </div>
              ) : null}
            </main>

            <aside className="color-workbench__inspector">
              <div className="color-workbench__harmonies">
                {harmonies.map((harmony) => (
                  <div key={harmony.label}>
                    <span>{harmony.label}</span>
                    <div>
                      {harmony.colors.map((harmonyColor, colorIndex) => (
                        <button
                          aria-label={`选择 ${harmony.label} ${harmonyColor}`}
                          key={`${harmony.label}:${colorIndex}:${harmonyColor}`}
                          onClick={() => chooseColor(harmonyColor)}
                          style={{ backgroundColor: harmonyColor }}
                          type="button"
                        />
                      ))}
                    </div>
                  </div>
                ))}
              </div>
              <div className="color-workbench__current" style={{ backgroundColor: normalizedColor, color: readableTextColor(normalizedColor) }}>
                <span>当前颜色</span><strong>{normalizedColor}</strong>
              </div>
              <div className="color-workbench__formats">
                {formats.map((entry) => (
                  <button key={entry.label} onClick={() => void onCopy(entry.value, entry.label)} type="button">
                    <span>{entry.label}</span><strong>{entry.value}</strong><Copy size={14} />
                  </button>
                ))}
              </div>
              <button className="color-workbench__confirm" onClick={() => void onCopy(formats[0].value, "HEX")} type="button">
                <Check size={17} />复制 HEX
              </button>
            </aside>
          </>
        ) : (
          <main className="color-workbench__collection">
            <div>
              <p className="eyebrow">{view === "palettes" ? "APPLE SYSTEM COLORS" : "LOCAL FAVORITES"}</p>
              <h2>{view === "palettes" ? "Apple 高饱和功能色" : "收藏颜色"}</h2>
              <p>{view === "palettes" ? "用于状态、操作与层级的 iHub 唯一色彩基线。" : "只保存在当前设备；点击任意颜色继续编辑。"}</p>
            </div>
            <div className="color-workbench__swatch-grid">
              {(view === "palettes" ? applePalette : favorites.map((favorite) => ({ color: favorite, name: favorite }))).map((entry) => (
                <article key={entry.color}>
                  <button onClick={() => chooseColor(entry.color)} style={{ backgroundColor: entry.color, color: readableTextColor(entry.color) }} type="button">
                    <strong>{entry.name}</strong><span>{entry.color}</span>
                  </button>
                  {view === "favorites" ? (
                    <button aria-label={`移除收藏 ${entry.color}`} className="color-workbench__remove-favorite" onClick={() => setFavorites((current) => current.filter((favorite) => favorite !== entry.color))} type="button">
                      <Trash2 size={14} />
                    </button>
                  ) : null}
                </article>
              ))}
            </div>
            {view === "favorites" && favorites.length === 0 ? (
              <button className="color-workbench__empty-favorite" onClick={toggleFavorite} type="button"><Plus size={16} />收藏当前颜色 {normalizedColor}</button>
            ) : null}
          </main>
        )}
      </div>
    </section>
  );
}
