import { describe, expect, it } from "vitest";
import {
  canInstallDiscoveredUpdate,
  recordAutomaticUpdateAttempt,
  type UpdateInstallEligibility,
} from "./update-install-policy";

function policy(overrides: Partial<UpdateInstallEligibility> = {}): UpdateInstallEligibility {
  return {
    automaticAttemptedVersions: new Set<string>(),
    automaticEnabled: true,
    candidateIsCurrent: true,
    checkInFlight: false,
    developmentBuild: false,
    desktop: true,
    discoveryActive: true,
    installed: false,
    installInFlight: false,
    origin: "automatic",
    version: "1.2.3",
    ...overrides,
  };
}

describe("automatic signed update installation policy", () => {
  it("is disabled by default until the user explicitly opts in", () => {
    expect(canInstallDiscoveredUpdate(policy({ automaticEnabled: false }))).toBe(false);
  });

  it.each([
    ["browser preview", { desktop: false }],
    ["development launcher", { developmentBuild: true }],
  ])("never auto-installs in %s", (_label, overrides) => {
    expect(canInstallDiscoveredUpdate(policy(overrides))).toBe(false);
  });

  it("does not install after a check when the live preference was switched off", () => {
    // App.tsx passes the current preference ref here after clearing its
    // check-in-flight ref; either condition independently blocks an install.
    expect(canInstallDiscoveredUpdate(policy({
      automaticEnabled: false,
      checkInFlight: false,
    }))).toBe(false);
    expect(canInstallDiscoveredUpdate(policy({
      automaticEnabled: true,
      checkInFlight: true,
    }))).toBe(false);
  });

  it("does not retry an automatic attempt for the same release version", () => {
    const attempts = new Set(["1.2.3"]);
    expect(canInstallDiscoveredUpdate(policy({ automaticAttemptedVersions: attempts }))).toBe(false);
    expect(recordAutomaticUpdateAttempt(attempts, "1.2.3")).toEqual(["1.2.3"]);
  });

  it("keeps the manual retry path available after an automatic attempt", () => {
    expect(canInstallDiscoveredUpdate(policy({
      automaticAttemptedVersions: new Set(["1.2.3"]),
      automaticEnabled: false,
      origin: "manual",
    }))).toBe(true);
  });
});
