/** Date and number helpers. The API gives UTC; people see their own time zone unless told otherwise. */
export function fmtDateTime(iso: string | null | undefined, tz?: string): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return new Intl.DateTimeFormat(undefined, {
    dateStyle: "medium",
    timeStyle: "short",
    ...(tz ? { timeZone: tz } : {}),
  }).format(d);
}

export function fmtDate(iso: string | null | undefined): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  return new Intl.DateTimeFormat(undefined, { dateStyle: "medium" }).format(d);
}

export function fmtAgo(iso: string | null | undefined, now = Date.now()): string {
  if (!iso) return "never";
  const secs = Math.round((now - new Date(iso).getTime()) / 1000);
  if (secs < 45) return "just now";
  if (secs < 3600) return `${Math.round(secs / 60)} min ago`;
  if (secs < 86400) return `${Math.round(secs / 3600)} h ago`;
  if (secs < 86400 * 30) return `${Math.round(secs / 86400)} d ago`;
  return fmtDate(iso);
}

export const fmtInt = (n: number | null | undefined) => (n ?? 0).toLocaleString();

/** A rate between 0 and 1 as a percent. */
export function fmtPct(rate: number | null | undefined): string {
  const r = rate ?? 0;
  return `${(r * 100).toFixed(r > 0 && r < 0.1 ? 1 : 0)}%`;
}

export function fullName(p: { first_name?: string | null; last_name?: string | null }): string {
  return [p.first_name, p.last_name].filter(Boolean).join(" ");
}

export function browserTimezone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
}

export function timezones(): string[] {
  try {
    const f = (Intl as unknown as { supportedValuesOf?: (k: string) => string[] })
      .supportedValuesOf;
    const list = f ? f("timeZone") : [];
    return list.includes("UTC") ? list : ["UTC", ...list];
  } catch {
    return ["UTC"];
  }
}

export function downloadText(name: string, text: string): void {
  const url = URL.createObjectURL(new Blob([text], { type: "text/plain" }));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  URL.revokeObjectURL(url);
}
