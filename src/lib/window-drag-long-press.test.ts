import { describe, expect, it, vi } from "vitest";
import {
  createLongPressWindowDragController,
  supportsLongPressWindowDrag,
  WINDOW_DRAG_LONG_PRESS_MS,
} from "./window-drag-long-press";

function createManualScheduler() {
  const pending = new Map<number, () => void>();
  let nextId = 1;
  return {
    cancel: vi.fn((timer: ReturnType<typeof setTimeout>) => {
      pending.delete(timer as unknown as number);
    }),
    flush() {
      const callbacks = [...pending.values()];
      pending.clear();
      callbacks.forEach((callback) => callback());
    },
    pendingCount() {
      return pending.size;
    },
    schedule: vi.fn((callback: () => void) => {
      const id = nextId++;
      pending.set(id, callback);
      return id as unknown as ReturnType<typeof setTimeout>;
    }),
  };
}

describe("long-press window drag", () => {
  it("accepts only a primary mouse, touch or pen-tip press", () => {
    expect(supportsLongPressWindowDrag({ button: 0, isPrimary: true, pointerType: "mouse" })).toBe(true);
    expect(supportsLongPressWindowDrag({ button: 0, isPrimary: true, pointerType: "touch" })).toBe(true);
    expect(supportsLongPressWindowDrag({ button: 0, isPrimary: true, pointerType: "pen" })).toBe(true);
    expect(supportsLongPressWindowDrag({ button: 2, isPrimary: true, pointerType: "mouse" })).toBe(false);
    expect(supportsLongPressWindowDrag({ button: 0, isPrimary: false, pointerType: "touch" })).toBe(false);
    expect(supportsLongPressWindowDrag({ button: 0, isPrimary: true, pointerType: "" })).toBe(false);
  });

  it("uses an explicit compact long-press threshold", () => {
    expect(WINDOW_DRAG_LONG_PRESS_MS).toBe(280);
  });

  it("does not trigger for a short click and clears its timer", () => {
    const scheduler = createManualScheduler();
    const onTrigger = vi.fn();
    const onPendingChange = vi.fn();
    const controller = createLongPressWindowDragController({
      cancelScheduled: scheduler.cancel,
      onPendingChange,
      onTrigger,
      schedule: scheduler.schedule,
    });

    expect(controller.begin({ pointerId: 4, x: 80, y: 5 })).toBe(true);
    controller.cancel(4);
    scheduler.flush();

    expect(onTrigger).not.toHaveBeenCalled();
    expect(onPendingChange.mock.calls).toEqual([[true], [false]]);
    expect(scheduler.pendingCount()).toBe(0);
  });

  it("cancels after meaningful movement while tolerating tiny pointer drift", () => {
    const scheduler = createManualScheduler();
    const onTrigger = vi.fn();
    const controller = createLongPressWindowDragController({
      cancelScheduled: scheduler.cancel,
      onTrigger,
      schedule: scheduler.schedule,
    });

    controller.begin({ pointerId: 7, x: 50, y: 4 });
    controller.move({ pointerId: 7, x: 53, y: 8 });
    expect(scheduler.pendingCount()).toBe(1);
    controller.move({ pointerId: 7, x: 57, y: 4 });
    scheduler.flush();

    expect(onTrigger).not.toHaveBeenCalled();
  });

  it("triggers at most once until the active pointer is released", () => {
    const scheduler = createManualScheduler();
    const onTrigger = vi.fn();
    const controller = createLongPressWindowDragController({
      cancelScheduled: scheduler.cancel,
      onTrigger,
      schedule: scheduler.schedule,
    });

    expect(controller.begin({ pointerId: 11, x: 50, y: 4 })).toBe(true);
    expect(controller.begin({ pointerId: 12, x: 50, y: 4 })).toBe(false);
    scheduler.flush();
    expect(onTrigger).toHaveBeenCalledTimes(1);
    expect(controller.begin({ pointerId: 11, x: 50, y: 4 })).toBe(false);

    controller.cancel(11);
    expect(controller.begin({ pointerId: 12, x: 50, y: 4 })).toBe(true);
    scheduler.flush();
    expect(onTrigger).toHaveBeenCalledTimes(2);
  });

  it("ignores unrelated pointer cleanup and disposes without a late trigger", () => {
    const scheduler = createManualScheduler();
    const onTrigger = vi.fn();
    const controller = createLongPressWindowDragController({
      cancelScheduled: scheduler.cancel,
      onTrigger,
      schedule: scheduler.schedule,
    });

    controller.begin({ pointerId: 20, x: 50, y: 4 });
    controller.cancel(21);
    expect(scheduler.pendingCount()).toBe(1);
    controller.dispose();
    scheduler.flush();

    expect(onTrigger).not.toHaveBeenCalled();
    expect(scheduler.pendingCount()).toBe(0);
    expect(controller.begin({ pointerId: 20, x: 50, y: 4 })).toBe(false);
  });
});
