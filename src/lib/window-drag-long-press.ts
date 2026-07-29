export const WINDOW_DRAG_LONG_PRESS_MS = 280;
export const WINDOW_DRAG_MOVE_TOLERANCE_PX = 6;

interface PointerPosition {
  pointerId: number;
  x: number;
  y: number;
}

interface WindowDragPointer {
  button: number;
  isPrimary: boolean;
  pointerType: string;
}

interface LongPressWindowDragOptions {
  onPendingChange?: (pending: boolean) => void;
  onTrigger: () => void;
  schedule?: (callback: () => void, delayMs: number) => ReturnType<typeof setTimeout>;
  cancelScheduled?: (timer: ReturnType<typeof setTimeout>) => void;
  delayMs?: number;
  moveTolerancePx?: number;
}

export interface LongPressWindowDragController {
  begin: (pointer: PointerPosition) => boolean;
  cancel: (pointerId?: number) => void;
  dispose: () => void;
  move: (pointer: PointerPosition) => void;
}

/**
 * Window dragging is intentionally restricted to the primary pointer. Pointer
 * down events report button 0 for the mouse, touch contact and pen tip.
 */
export function supportsLongPressWindowDrag(pointer: WindowDragPointer) {
  return pointer.isPrimary && pointer.button === 0
    && (pointer.pointerType === "mouse"
      || pointer.pointerType === "touch"
      || pointer.pointerType === "pen");
}

/**
 * A small, framework-independent gesture controller keeps the native drag
 * side effect deterministic: one active pointer, one timer and at most one
 * trigger until that pointer is released or cancelled.
 */
export function createLongPressWindowDragController({
  onPendingChange,
  onTrigger,
  schedule = (callback, delayMs) => setTimeout(callback, delayMs),
  cancelScheduled = (timer) => clearTimeout(timer),
  delayMs = WINDOW_DRAG_LONG_PRESS_MS,
  moveTolerancePx = WINDOW_DRAG_MOVE_TOLERANCE_PX,
}: LongPressWindowDragOptions): LongPressWindowDragController {
  let activePointerId: number | null = null;
  let startPoint: { x: number; y: number } | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let triggered = false;
  let disposed = false;

  const setPending = (pending: boolean) => {
    if (!disposed) {
      onPendingChange?.(pending);
    }
  };

  const clearTimer = () => {
    if (timer !== null) {
      cancelScheduled(timer);
      timer = null;
    }
  };

  const reset = (notify = true) => {
    clearTimer();
    activePointerId = null;
    startPoint = null;
    triggered = false;
    if (notify) {
      setPending(false);
    }
  };

  return {
    begin(pointer) {
      if (disposed || activePointerId !== null) {
        return false;
      }

      activePointerId = pointer.pointerId;
      startPoint = { x: pointer.x, y: pointer.y };
      triggered = false;
      setPending(true);
      timer = schedule(() => {
        timer = null;
        if (disposed || activePointerId !== pointer.pointerId || triggered) {
          return;
        }
        triggered = true;
        startPoint = null;
        setPending(false);
        onTrigger();
      }, delayMs);
      return true;
    },

    move(pointer) {
      if (disposed || triggered || pointer.pointerId !== activePointerId || !startPoint) {
        return;
      }
      if (Math.hypot(pointer.x - startPoint.x, pointer.y - startPoint.y) > moveTolerancePx) {
        reset();
      }
    },

    cancel(pointerId) {
      if (disposed || (pointerId !== undefined && pointerId !== activePointerId)) {
        return;
      }
      reset();
    },

    dispose() {
      if (disposed) {
        return;
      }
      clearTimer();
      activePointerId = null;
      startPoint = null;
      triggered = false;
      disposed = true;
    },
  };
}
