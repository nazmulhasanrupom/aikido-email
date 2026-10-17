/** `next` is where to go after login. Only a path inside this app is allowed. */
export function safeNext(next: unknown): string | undefined {
  if (typeof next !== "string") return undefined;
  if (!next.startsWith("/") || next.startsWith("//") || next.includes("\\")) return undefined;
  if (next.startsWith("/login") || next.startsWith("/setup")) return undefined;
  return next;
}

export const HOME = "/w/default/campaigns";
