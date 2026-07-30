import { readFileSync } from "node:fs";
import { describe, expect, it } from "vitest";

const appSource = readFileSync(new URL("../App.tsx", import.meta.url), "utf8");
const permissionSource = readFileSync(
  new URL("../../src-tauri/permissions/app-commands.toml", import.meta.url),
  "utf8",
);

describe("host log settings boundary", () => {
  it("exposes refresh, copy, clear, and a keyboard-scrollable log region", () => {
    expect(appSource).toContain('id="host-log-title">滚动诊断日志');
    expect(appSource).toContain('aria-label="诊断日志操作"');
    expect(appSource).toContain('aria-label="iHub 脱敏诊断日志"');
    expect(appSource).toContain('role="log"');
    expect(appSource).toContain("refreshHostDiagnostics(true)");
    expect(appSource).toContain("copyHostDiagnostics()");
    expect(appSource).toContain("clearHostDiagnostics()");
    expect(appSource).toContain("hostLogCoordinator.readInBackground()");
    expect(appSource).toContain("const pollAfterCompletion = async () =>");
    expect(appSource).toContain("timeout = window.setTimeout(() =>");
    expect(appSource).not.toContain("refreshHostDiagnostics(false, true)");
    const pollingEffect = appSource.match(
      /const pollAfterCompletion = async \(\) => \{[\s\S]*?\n  \}, \[hostLogCoordinator, publishHostLog, settingsOpen\]\);/,
    )?.[0] ?? "";
    expect(pollingEffect).not.toContain("setInterval");
    expect(appSource).toContain("hostLogViewportRef");
    expect(appSource).toContain("if (hostLogFollowTailRef.current)");
    expect(appSource).toContain("viewport.scrollTop = viewport.scrollHeight");
    expect(appSource).toContain("每 3 秒刷新，向上滚动时暂停跟随");
  });

  it("keeps read and clear commands out of detached plugin hosts", () => {
    const main = permissionSource.match(
      /identifier\s*=\s*"main-app-commands"[\s\S]*?commands\.allow\s*=\s*\[([\s\S]*?)\]/,
    )?.[1] ?? "";
    const detached = permissionSource.match(
      /identifier\s*=\s*"detached-plugin-host-commands"[\s\S]*?commands\.allow\s*=\s*\[([\s\S]*?)\]/,
    )?.[1] ?? "";
    expect(main).toContain('"get_host_log"');
    expect(main).toContain('"clear_host_log"');
    expect(detached).not.toContain('"get_host_log"');
    expect(detached).not.toContain('"clear_host_log"');
  });
});
