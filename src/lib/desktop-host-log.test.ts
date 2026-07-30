import { beforeEach, describe, expect, it, vi } from "vitest";

const invokeMock = vi.hoisted(() => vi.fn());

vi.mock("@tauri-apps/api/core", () => ({
  invoke: invokeMock,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(),
}));

import { clearHostLog, readHostLog } from "./desktop";

describe("browser host-log bridge boundary", () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  it("reads and clears only the in-memory fixture without invoking Tauri", async () => {
    const snapshot = await readHostLog();
    expect(snapshot.entries).toHaveLength(3);

    const cleared = await clearHostLog(snapshot);
    expect(cleared.entries).toEqual([]);
    expect(cleared.totalBytes).toBe(0);
    expect(invokeMock).not.toHaveBeenCalled();
  });
});
