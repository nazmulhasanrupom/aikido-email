/**
 * A mirror of the Rust template parser (`src/followups/template.rs`).
 * Syntax: `{{name}}`, `{{name|fallback}}`, `{{ai:key}}`, `{{ai:key|fallback}}`. Spaces inside the braces are ignored.
 */
export type Part =
  | { kind: "text"; text: string }
  | { kind: "var"; name: string; fallback: string | null; raw: string }
  | { kind: "ai"; key: string; fallback: string | null; raw: string };

export class TemplateError extends Error {}

export const BUILTIN_VARS = [
  "email",
  "sender_name",
  "sender_email",
  "unsubscribe",
  "unsubscribe_url",
];

export function validName(name: string): boolean {
  return /^[A-Za-z_][A-Za-z0-9_]*$/.test(name);
}

export function parse(src: string): Part[] {
  const parts: Part[] = [];
  let rest = src;
  for (;;) {
    const open = rest.indexOf("{{");
    if (open < 0) break;
    if (open > 0) parts.push({ kind: "text", text: rest.slice(0, open) });
    const after = rest.slice(open + 2);
    const close = after.indexOf("}}");
    if (close < 0) throw new TemplateError("A '{{' has no closing '}}'.");
    const inner = after.slice(0, close);
    const bar = inner.indexOf("|");
    const name = (bar < 0 ? inner : inner.slice(0, bar)).trim();
    const fallback = bar < 0 ? null : inner.slice(bar + 1).trim();
    const raw = `{{${inner}}}`;
    if (name.startsWith("ai:")) {
      const key = name.slice(3).trim();
      if (!validName(key))
        throw new TemplateError(`'${raw}' is not valid. An AI part is written {{ai:key}}.`);
      parts.push({ kind: "ai", key, fallback, raw });
    } else {
      if (!validName(name))
        throw new TemplateError(
          `'${raw}' is not valid. A variable name has letters, digits and '_', and starts with a letter.`,
        );
      parts.push({ kind: "var", name, fallback, raw });
    }
    rest = after.slice(close + 2);
  }
  if (rest) parts.push({ kind: "text", text: rest });
  return parts;
}

/** Variables used: name and whether it has a fallback. A name can repeat. */
export function variables(src: string): { name: string; hasFallback: boolean; raw: string }[] {
  return parse(src).flatMap((p) =>
    p.kind === "var" ? [{ name: p.name, hasFallback: p.fallback !== null, raw: p.raw }] : [],
  );
}

export function aiKeys(src: string): string[] {
  return parse(src).flatMap((p) => (p.kind === "ai" ? [p.key] : []));
}

/** Names with no value in any case: used without a fallback (builtins are always there). */
export function required(src: string): string[] {
  const names = variables(src)
    .filter((v) => !v.hasFallback && !BUILTIN_VARS.includes(v.name))
    .map((v) => v.name);
  return [...new Set(names)].sort();
}

/** Same text, with the fallback of every `{{name}}` / `{{name|..}}` that reads `raw` set to `fallback`. */
export function withFallback(src: string, raw: string, fallback: string): string {
  const name =
    raw
      .replace(/^\{\{\s*/, "")
      .replace(/\s*\}\}$/, "")
      .split("|")[0]
      ?.trim() ?? "";
  return src.split(raw).join(fallback ? `{{${name}|${fallback}}}` : `{{${name}}}`);
}

/** Fill a template for a preview. A missing value with no fallback stays visible as [name]. */
export function renderPreview(src: string, values: Record<string, string>): string {
  try {
    return parse(src)
      .map((p) => {
        if (p.kind === "text") return p.text;
        if (p.kind === "ai") return values[`ai:${p.key}`] || p.fallback || `[AI: ${p.key}]`;
        return values[p.name]?.trim() || p.fallback || `[${p.name}]`;
      })
      .join("");
  } catch {
    return src;
  }
}
