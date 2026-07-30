const WINDOWS_VERBATIM_UNC_PREFIX = "\\\\?\\UNC\\";
const WINDOWS_VERBATIM_PREFIX = "\\\\?\\";

/**
 * Removes Windows' host-only verbatim path spelling from text shown to a
 * person. Callers keep the original value for native commands and identity
 * checks; this helper is deliberately presentation-only.
 */
export function displayLocalPath(value: string): string {
  return value
    .replaceAll(WINDOWS_VERBATIM_UNC_PREFIX, "\\\\")
    .replaceAll(WINDOWS_VERBATIM_PREFIX, "");
}
