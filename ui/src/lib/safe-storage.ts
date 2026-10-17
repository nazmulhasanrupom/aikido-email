/**
 * localStorage can be missing or throw (private windows, blocked site data).
 * Use it only for small per-viewer conveniences. Never for secrets.
 */
export function readStored(key: string): string | null {
  try {
    return window.localStorage.getItem(key);
  } catch {
    return null;
  }
}

export function writeStored(key: string, value: string): void {
  try {
    window.localStorage.setItem(key, value);
  } catch {
    // not saved; the page still works
  }
}
