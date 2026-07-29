import { describe, expect, it } from "vitest";
import {
  createSimulatedLiveColorSample,
  initialLiveColorPickerState,
  normalizeLiveColorSample,
  transitionLiveColorPicker,
  validateLiveColorPickerSession,
} from "./live-color-picker";

describe("live color picker state machine", () => {
  it("does not confirm until the starting mouse click has been released", () => {
    const held = {
      ...createSimulatedLiveColorSample(0),
      leftPressed: true,
    };
    let state = transitionLiveColorPicker(initialLiveColorPickerState, { type: "start" });
    state = transitionLiveColorPicker(state, { type: "started" });
    state = transitionLiveColorPicker(state, { type: "sample", sample: held });
    expect(state.phase).toBe("sampling");
    expect(state.armed).toBe(false);

    state = transitionLiveColorPicker(state, {
      type: "sample",
      sample: createSimulatedLiveColorSample(1),
    });
    expect(state.armed).toBe(true);
    state = transitionLiveColorPicker(state, { type: "sample", sample: held });
    expect(state.phase).toBe("confirmed");
  });

  it("gives right click and Escape cancellation precedence", () => {
    let state = transitionLiveColorPicker(initialLiveColorPickerState, { type: "start" });
    state = transitionLiveColorPicker(state, { type: "started" });
    state = transitionLiveColorPicker(state, {
      type: "sample",
      sample: createSimulatedLiveColorSample(2),
    });
    state = transitionLiveColorPicker(state, {
      type: "sample",
      sample: {
        ...createSimulatedLiveColorSample(3),
        leftPressed: true,
        rightPressed: true,
      },
    });
    expect(state.phase).toBe("cancelled");
  });

  it("rejects malformed, oversized or center-mismatched pixel grids", () => {
    const valid = createSimulatedLiveColorSample(4);
    expect(normalizeLiveColorSample(valid).pixels).toHaveLength(81);
    expect(() => normalizeLiveColorSample({ ...valid, pixels: valid.pixels.slice(1) })).toThrow();
    expect(() => normalizeLiveColorSample({
      ...valid,
      pixels: valid.pixels.map((pixel, index) => index === 40 ? "#000000" : pixel),
    })).toThrow();
  });

  it("supports explicit foreground confirmation and cancellation", () => {
    let state = transitionLiveColorPicker(initialLiveColorPickerState, { type: "start" });
    state = transitionLiveColorPicker(state, { type: "started" });
    state = transitionLiveColorPicker(state, {
      type: "sample",
      sample: createSimulatedLiveColorSample(5),
    });
    expect(transitionLiveColorPicker(state, { type: "confirm" }).phase).toBe("confirmed");
    expect(transitionLiveColorPicker(state, { type: "cancel" }).phase).toBe("cancelled");
  });

  it("bounds session tokens, cadence and lifetime before starting a poll", () => {
    const valid = {
      sessionId: "session-a",
      sampleEdge: 9,
      minimumIntervalMs: 72,
      expiresAfterMs: 30_000,
    };
    expect(validateLiveColorPickerSession(valid)).toEqual(valid);
    expect(() => validateLiveColorPickerSession({
      ...valid,
      minimumIntervalMs: 10,
    })).toThrow();
    expect(() => validateLiveColorPickerSession({
      ...valid,
      sessionId: "x".repeat(65),
    })).toThrow();
  });
});
