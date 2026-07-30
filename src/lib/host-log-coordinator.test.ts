import { describe, expect, it, vi } from "vitest";
import { HostLogCoordinator } from "./host-log-coordinator";

function deferred<T>() {
  let resolve!: (value: T | PromiseLike<T>) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((resolvePromise, rejectPromise) => {
    resolve = resolvePromise;
    reject = rejectPromise;
  });
  return { promise, reject, resolve };
}

describe("HostLogCoordinator", () => {
  it("single-flights overlapping background and foreground reads", async () => {
    const pendingRead = deferred<string>();
    const read = vi.fn(() => pendingRead.promise);
    const coordinator = new HostLogCoordinator(read, vi.fn());
    coordinator.openSession();

    const background = coordinator.readInBackground();
    const foreground = coordinator.startForeground(
      "refresh",
      ({ read: readForeground }) => readForeground(),
    );

    expect(foreground.started).toBe(true);
    expect(read).toHaveBeenCalledTimes(1);
    pendingRead.resolve("snapshot");

    await expect(background).resolves.toEqual({
      status: "applied",
      snapshot: "snapshot",
    });
    if (foreground.started) {
      await expect(foreground.promise).resolves.toEqual({
        status: "applied",
        snapshot: "snapshot",
      });
    }
  });

  it("gates polling and competing foreground actions synchronously", async () => {
    const releaseCopy = deferred<void>();
    const read = vi.fn(async () => "snapshot");
    const coordinator = new HostLogCoordinator(read, vi.fn());
    coordinator.openSession();

    const copy = coordinator.startForeground("copy", async () => {
      await releaseCopy.promise;
    });
    const blockedPoll = coordinator.readInBackground();
    const competingRefresh = coordinator.startForeground(
      "refresh",
      ({ read: readForeground }) => readForeground(),
    );

    expect(copy.started).toBe(true);
    expect(competingRefresh.started).toBe(false);
    await expect(blockedPoll).resolves.toEqual({ status: "blocked" });
    expect(read).not.toHaveBeenCalled();

    releaseCopy.resolve();
    if (copy.started) {
      await copy.promise;
    }
    await expect(coordinator.readInBackground()).resolves.toEqual({
      status: "applied",
      snapshot: "snapshot",
    });
  });

  it("invalidates an old read, drains it, then clears without stale overwrite", async () => {
    const pendingRead = deferred<string>();
    const pendingClear = deferred<string>();
    const events: string[] = [];
    const coordinator = new HostLogCoordinator(
      () => {
        events.push("read:start");
        return pendingRead.promise.then((snapshot) => {
          events.push("read:end");
          return snapshot;
        });
      },
      () => {
        events.push("clear:start");
        return pendingClear.promise.then((snapshot) => {
          events.push("clear:end");
          return snapshot;
        });
      },
    );
    coordinator.openSession();

    const oldRead = coordinator.readInBackground();
    const clear = coordinator.startClear("visible-before-clear");
    expect(clear.started).toBe(true);
    expect(events).toEqual(["read:start"]);
    await expect(coordinator.readInBackground()).resolves.toEqual({
      status: "blocked",
    });

    pendingRead.resolve("stale-before-clear");
    await expect(oldRead).resolves.toEqual({ status: "stale" });
    await vi.waitFor(() => {
      expect(events).toEqual(["read:start", "read:end", "clear:start"]);
    });

    pendingClear.resolve("empty-after-clear");
    if (clear.started) {
      await expect(clear.promise).resolves.toEqual({
        status: "applied",
        snapshot: "empty-after-clear",
      });
    }
    expect(events).toEqual([
      "read:start",
      "read:end",
      "clear:start",
      "clear:end",
    ]);
  });

  it("invalidates a response when the settings session closes", async () => {
    const pendingRead = deferred<string>();
    const coordinator = new HostLogCoordinator(
      () => pendingRead.promise,
      vi.fn(),
    );
    coordinator.openSession();

    const read = coordinator.readInBackground();
    coordinator.closeSession();
    pendingRead.resolve("late");

    await expect(read).resolves.toEqual({ status: "stale" });
  });

  it("marks an in-flight foreground operation stale after cleanup", async () => {
    const pendingCopy = deferred<void>();
    const coordinator = new HostLogCoordinator(
      vi.fn(async () => "snapshot"),
      vi.fn(),
    );
    coordinator.openSession();

    const copy = coordinator.startForeground("copy", async ({ isCurrent }) => {
      await pendingCopy.promise;
      return isCurrent();
    });
    expect(copy.started).toBe(true);
    coordinator.closeSession();
    pendingCopy.resolve();

    if (copy.started) {
      await expect(copy.promise).resolves.toBe(false);
    }
  });

  it("drains a closed session read before serving a reopened session", async () => {
    const firstRead = deferred<string>();
    const secondRead = deferred<string>();
    const readSnapshot = vi.fn()
      .mockImplementationOnce(() => firstRead.promise)
      .mockImplementationOnce(() => secondRead.promise);
    const coordinator = new HostLogCoordinator(readSnapshot, vi.fn());
    coordinator.openSession();
    const oldSessionRead = coordinator.readInBackground();

    coordinator.closeSession();
    coordinator.openSession();
    const reopenedSessionRead = coordinator.readInBackground();
    expect(readSnapshot).toHaveBeenCalledTimes(1);

    firstRead.resolve("old");
    await expect(oldSessionRead).resolves.toEqual({ status: "stale" });
    await vi.waitFor(() => {
      expect(readSnapshot).toHaveBeenCalledTimes(2);
    });
    secondRead.resolve("new");
    await expect(reopenedSessionRead).resolves.toEqual({
      status: "applied",
      snapshot: "new",
    });
  });

  it("releases a rejected read flight so the next poll can recover", async () => {
    const readSnapshot = vi.fn()
      .mockRejectedValueOnce(new Error("unreadable retained file"))
      .mockResolvedValueOnce("recovered");
    const coordinator = new HostLogCoordinator(readSnapshot, vi.fn());
    coordinator.openSession();

    await expect(coordinator.readInBackground()).rejects.toThrow(
      "unreadable retained file",
    );
    await expect(coordinator.readInBackground()).resolves.toEqual({
      status: "applied",
      snapshot: "recovered",
    });
    expect(readSnapshot).toHaveBeenCalledTimes(2);
  });

  it("releases the foreground gate after a rejected clear", async () => {
    const clearSnapshots = vi.fn()
      .mockRejectedValueOnce(new Error("clear failed"))
      .mockResolvedValueOnce("empty");
    const coordinator = new HostLogCoordinator(
      vi.fn(async () => "snapshot"),
      clearSnapshots,
    );
    coordinator.openSession();

    const failed = coordinator.startClear();
    expect(failed.started).toBe(true);
    if (failed.started) {
      await expect(failed.promise).rejects.toThrow("clear failed");
    }

    const recovered = coordinator.startClear();
    expect(recovered.started).toBe(true);
    if (recovered.started) {
      await expect(recovered.promise).resolves.toEqual({
        status: "applied",
        snapshot: "empty",
      });
    }
  });
});
