import { describe, expect, it } from "vitest";
import { launcherSystemCommandResults } from "./launcher-system-commands";

describe("launcher system commands", () => {
  it("keeps settings discoverable without adding it to the empty home grid", () => {
    expect(launcherSystemCommandResults("")).toEqual([]);
    expect(launcherSystemCommandResults("设置")).toEqual([
      expect.objectContaining({
        name: "偏好设置",
        commandId: "ihub.open-settings",
      }),
    ]);
    expect(launcherSystemCommandResults("autostart")[0]?.commandId).toBe("ihub.open-settings");
  });
});
