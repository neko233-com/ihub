export const WINDOW_DRAG_LONG_PRESS_MS = 280;
export const WINDOW_DRAG_MOVE_TOLERANCE_PX = 6;

interface PointerPosition {
  pointerId: number;
  x: number;
  y: number;
}

interface WindowDragStart extends PointerPosition {
  /** Mouse dragging starts as soon as intentional movement is detected. */
  triggerOnMove: boolean;
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
  begin: (pointer: WindowDragStart) => boolean;
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
  let triggerOnMove = false;
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
    triggerOnMove = false;
    if (notify) {
      setPending(false);
    }
  };

  const trigger = () => {
    if (disposed || activePointerId === null || triggered) {
      return;
    }
    clearTimer();
    triggered = true;
    startPoint = null;
    setPending(false);
    onTrigger();
  };

  return {
    begin(pointer) {
      if (disposed || activePointerId !== null) {
        return false;
      }

      activePointerId = pointer.pointerId;
      startPoint = { x: pointer.x, y: pointer.y };
      triggered = false;
      triggerOnMove = pointer.triggerOnMove;
      setPending(true);
      timer = schedule(() => {
        timer = null;
        if (disposed || activePointerId !== pointer.pointerId || triggered) {
          return;
        }
        trigger();
      }, delayMs);
      return true;
    },

    move(pointer) {
      if (disposed || triggered || pointer.pointerId !== activePointerId || !startPoint) {
        return;
      }
      if (Math.hypot(pointer.x - startPoint.x, pointer.y - startPoint.y) > moveTolerancePx) {
        if (triggerOnMove) {
          trigger();
        } else {
          reset();
        }
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
      triggerOnMove = false;
      disposed = true;
    },
  };
}
