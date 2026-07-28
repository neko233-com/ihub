/**
 * The updater's native handle is deliberately kept out of this module. That
 * makes the decision reproducible in tests while App.tsx supplies the live
 * refs that protect it from an in-flight check replacing the handle.
 */
export type UpdateInstallOrigin = "manual" | "automatic";

export interface UpdateInstallEligibility {
  automaticAttemptedVersions: ReadonlySet<string>;
  automaticEnabled: boolean;
  candidateIsCurrent: boolean;
  checkInFlight: boolean;
  developmentBuild: boolean;
  desktop: boolean;
  discoveryActive: boolean;
  installed: boolean;
  installInFlight: boolean;
  origin: UpdateInstallOrigin;
  version: string;
}

/**
 * Manual installation deliberately ignores the automatic preference and its
 * attempt ledger. This leaves a safe recovery route after an automatic
 * download/install attempt fails for a particular release.
 */
export function canInstallDiscoveredUpdate(input: UpdateInstallEligibility): boolean {
  if (
    !input.desktop
    || input.developmentBuild
    || !input.discoveryActive
    || input.checkInFlight
    || input.installInFlight
    || input.installed
    || !input.candidateIsCurrent
  ) {
    return false;
  }

  if (input.origin === "manual") {
    return true;
  }

  return input.automaticEnabled && !input.automaticAttemptedVersions.has(input.version);
}

/**
 * Retain a small, stable history across application restarts. Moving a
 * repeated value to the end keeps the newest records when the cap is reached.
 */
export function recordAutomaticUpdateAttempt(
  attemptedVersions: ReadonlySet<string>,
  version: string,
  maximumEntries = 12,
): string[] {
  const boundedMaximum = Math.max(1, Math.floor(maximumEntries));
  const next = Array.from(attemptedVersions).filter((existing) => existing !== version);
  next.push(version);
  return next.slice(-boundedMaximum);
}
