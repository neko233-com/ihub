import { Check, Crop, LoaderCircle, RotateCcw, X } from "lucide-react";
import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type PointerEvent as ReactPointerEvent,
} from "react";
import {
  captureRegionStyle,
  cropCaptureRegion,
  isUsableCaptureRegion,
  pointInCaptureSource,
  regionFromDrag,
  type CapturePoint,
  type CaptureRect,
  type CroppedCapture,
  type RegionCaptureSource,
} from "../lib/region-capture";

interface RegionCaptureEditorProps {
  developmentPreview?: boolean;
  onCancel: () => void;
  onExport: (capture: CroppedCapture) => Promise<void> | void;
  onStatus?: (message: string) => void;
  source: RegionCaptureSource;
}

export function RegionCaptureEditor({
  developmentPreview = false,
  onCancel,
  onExport,
  onStatus,
  source,
}: RegionCaptureEditorProps) {
  const frameRef = useRef<HTMLDivElement | null>(null);
  const anchorRef = useRef<CapturePoint | null>(null);
  const pointerIdRef = useRef<number | null>(null);
  const [selection, setSelection] = useState<CaptureRect | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const [isExporting, setIsExporting] = useState(false);
  const frameWidth = useMemo(
    () => Math.round(Math.min(720, 360 * (source.width / source.height))),
    [source.height, source.width],
  );

  const pointFromPointer = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    const bounds = frameRef.current?.getBoundingClientRect();
    if (!bounds) {
      return { x: 0, y: 0 };
    }
    return pointInCaptureSource(
      { x: event.clientX, y: event.clientY },
      {
        x: bounds.left,
        y: bounds.top,
        width: bounds.width,
        height: bounds.height,
      },
      source,
    );
  }, [source]);

  const finishDrag = useCallback((event: ReactPointerEvent<HTMLDivElement>) => {
    if (pointerIdRef.current !== event.pointerId || !anchorRef.current) {
      return;
    }
    const next = regionFromDrag(anchorRef.current, pointFromPointer(event), source);
    setSelection(isUsableCaptureRegion(next) ? next : null);
    anchorRef.current = null;
    pointerIdRef.current = null;
    setIsDragging(false);
    try {
      event.currentTarget.releasePointerCapture(event.pointerId);
    } catch {
      // A browser may already have released capture after pointer cancellation.
    }
  }, [pointFromPointer, source]);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key !== "Escape") {
        return;
      }
      event.preventDefault();
      onCancel();
    };
    window.addEventListener("keydown", handleKeyDown, { capture: true });
    return () => window.removeEventListener("keydown", handleKeyDown, { capture: true });
  }, [onCancel]);

  const exportSelection = async () => {
    if (!isUsableCaptureRegion(selection) || isExporting) {
      return;
    }
    setIsExporting(true);
    try {
      const capture = await cropCaptureRegion(source, selection);
      await onExport(capture);
    } catch (error) {
      onStatus?.(error instanceof Error ? error.message : "未能导出截图选区。");
    } finally {
      setIsExporting(false);
    }
  };

  return (
    <section
      aria-label="矩形截图选区"
      className="region-capture-editor"
      data-state={isDragging ? "dragging" : selection ? "selected" : "ready"}
    >
      <header className="region-capture-editor__header">
        <div>
          <strong>拖拽选择截图区域</strong>
          <span>左键拖拽 · Esc 或右键取消</span>
        </div>
        {selection ? (
          <output aria-live="polite">
            {selection.width} × {selection.height}
          </output>
        ) : null}
      </header>

      <div className="region-capture-editor__workspace">
        <div
          aria-label="截图选区画面"
          className="region-capture-editor__frame"
          onContextMenu={(event) => {
            event.preventDefault();
            onCancel();
          }}
          onPointerCancel={(event) => {
            if (pointerIdRef.current !== event.pointerId) {
              return;
            }
            anchorRef.current = null;
            pointerIdRef.current = null;
            setIsDragging(false);
            setSelection(null);
          }}
          onPointerDown={(event) => {
            if (event.button === 2) {
              onCancel();
              return;
            }
            if (event.button !== 0 || !event.isPrimary || pointerIdRef.current !== null) {
              return;
            }
            event.preventDefault();
            pointerIdRef.current = event.pointerId;
            anchorRef.current = pointFromPointer(event);
            setSelection(regionFromDrag(anchorRef.current, anchorRef.current, source));
            setIsDragging(true);
            event.currentTarget.setPointerCapture(event.pointerId);
          }}
          onPointerMove={(event) => {
            if (pointerIdRef.current !== event.pointerId || !anchorRef.current) {
              return;
            }
            setSelection(regionFromDrag(anchorRef.current, pointFromPointer(event), source));
          }}
          onPointerUp={finishDrag}
          ref={frameRef}
          role="application"
          style={{
            aspectRatio: `${source.width} / ${source.height}`,
            width: `min(100%, ${frameWidth}px)`,
          }}
          tabIndex={0}
        >
          <img alt="" draggable={false} src={source.url} />
          <div aria-hidden="true" className="region-capture-editor__veil" />
          {selection && selection.width > 0 && selection.height > 0 ? (
            <div
              aria-hidden="true"
              className="region-capture-editor__selection"
              style={captureRegionStyle(selection, source)}
            >
              <span className="region-capture-editor__selection-size">
                {selection.width} × {selection.height}
              </span>
            </div>
          ) : null}
        </div>
      </div>

      <footer className="region-capture-editor__actions">
        {developmentPreview ? (
          <button
            className="toolbox-secondary-action"
            onClick={() => setSelection(regionFromDrag(
              { x: source.width * 0.2, y: source.height * 0.2 },
              { x: source.width * 0.8, y: source.height * 0.8 },
              source,
            ))}
            type="button"
          >
            <Crop size={14} />
            创建模拟选区（开发验证）
          </button>
        ) : null}
        <button
          className="toolbox-secondary-action"
          disabled={!selection || isExporting}
          onClick={() => setSelection(null)}
          type="button"
        >
          <RotateCcw size={14} />
          重选
        </button>
        <button
          className="toolbox-secondary-action"
          disabled={isExporting}
          onClick={onCancel}
          type="button"
        >
          <X size={14} />
          取消
        </button>
        <button
          className="toolbox-record-action region-capture-editor__export"
          disabled={!isUsableCaptureRegion(selection) || isExporting}
          onClick={() => void exportSelection()}
          type="button"
        >
          {isExporting ? <LoaderCircle className="spin" size={15} /> : selection ? <Check size={15} /> : <Crop size={15} />}
          {isExporting ? "正在裁剪…" : "导出选区 PNG"}
        </button>
      </footer>
    </section>
  );
}
