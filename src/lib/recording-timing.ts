/**
 * Calculates only the time during which MediaRecorder is actively recording.
 * A paused segment intentionally does not consume the built-in recorder limit.
 */
export function activeRecordingElapsedMs(
  committedActiveMs: number,
  activeStartedAt: number | null,
  now: number,
) {
  const committed = Number.isFinite(committedActiveMs)
    ? Math.max(0, committedActiveMs)
    : 0;
  if (activeStartedAt === null || !Number.isFinite(activeStartedAt) || !Number.isFinite(now)) {
    return committed;
  }
  return committed + Math.max(0, now - activeStartedAt);
}

export function remainingActiveRecordingMs(
  limitMs: number,
  committedActiveMs: number,
  activeStartedAt: number | null,
  now: number,
) {
  const limit = Number.isFinite(limitMs) ? Math.max(0, limitMs) : 0;
  return Math.max(0, limit - activeRecordingElapsedMs(committedActiveMs, activeStartedAt, now));
}
