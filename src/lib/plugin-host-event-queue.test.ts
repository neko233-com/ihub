import { describe, expect, it } from "vitest";
import {
  enqueueBoundedPluginHostEvent,
  rememberBoundedPluginEventId,
  restoreFailedPluginHostEventTail,
} from "./plugin-host-event-queue";

describe("bounded plugin host event retention", () => {
  it("drops the oldest not-yet-ready event at the hard queue limit", () => {
    const queue = [1, 2, 3];
    enqueueBoundedPluginHostEvent(queue, 4, 3);
    expect(queue).toEqual([2, 3, 4]);
  });

  it("caps dispatched event identities using insertion order", () => {
    const ids = new Set(["oldest", "middle"]);
    rememberBoundedPluginEventId(ids, "newest", 2);
    expect([...ids]).toEqual(["middle", "newest"]);
  });

  it("retains the failed event and its untouched tail in order", () => {
    const queue = ["arrived-during-flush"];
    const drained = ["sent", "failed", "tail-1", "tail-2"];

    restoreFailedPluginHostEventTail(queue, drained, 1, 4);

    expect(queue).toEqual(["failed", "tail-1", "tail-2", "arrived-during-flush"]);
  });

  it("keeps the newest bounded events when a failed tail merges with new events", () => {
    const queue = ["new-1", "new-2"];
    const drained = ["sent", "failed", "tail"];

    restoreFailedPluginHostEventTail(queue, drained, 1, 3);

    expect(queue).toEqual(["tail", "new-1", "new-2"]);
  });
});
