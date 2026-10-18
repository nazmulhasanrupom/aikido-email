/** Make a placeholder key from a column name: lower case, `_` for spaces and dashes, Latin letters and digits only. */
export function keyFromName(name: string): string {
  let s = name
    .trim()
    .toLowerCase()
    .replace(/[\s-]+/g, "_")
    .replace(/[^a-z0-9_]/g, "")
    .replace(/_+/g, "_");
  s = s.replace(/^[^a-z]+/, "").replace(/_+$/, "");
  return s.slice(0, 40).replace(/_+$/, "");
}

/** Same rule, but a name with no Latin letters gets `field_<n>`. */
export function keyOrFallback(name: string, n: number): string {
  return keyFromName(name) || `field_${n}`;
}

export const VALID_KEY = /^[a-z][a-z0-9_]{0,39}$/;

/** Guess a field kind from sample values. */
export function guessKind(samples: string[]): "text" | "number" | "url" | "date" {
  const s = samples.map((x) => x.trim()).filter(Boolean);
  if (s.length === 0) return "text";
  if (s.every((x) => /^-?\d+([.,]\d+)?$/.test(x))) return "number";
  if (s.every((x) => /^https?:\/\/\S+$/i.test(x) || /^www\.\S+\.\S+$/i.test(x))) return "url";
  if (s.every((x) => /^\d{4}-\d{2}-\d{2}/.test(x) || /^\d{1,2}[/.]\d{1,2}[/.]\d{2,4}$/.test(x)))
    return "date";
  return "text";
}
