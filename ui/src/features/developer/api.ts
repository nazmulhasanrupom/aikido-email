import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { authStateKey } from "@/api/auth";
import { messageOf } from "@/api/errors";
import { http } from "@/api/http";
import { useUser } from "@/features/settings/useUser";

export interface LogEntry {
  id: number;
  at: string;
  level: "ERROR" | "WARN" | "INFO" | "DEBUG" | "TRACE";
  target: string;
  message: string;
  fields: string;
  span: string | null;
}

export interface LogsReply {
  entries: LogEntry[];
  last_id: number;
  buffer_size: number;
  filter: string;
}

export interface SqlReply {
  columns: string[];
  rows: (string | null)[][];
  row_count: number;
  truncated: boolean;
  rows_affected: number;
  elapsed_ms: number;
  mode: "read" | "preview" | "commit";
  committed: boolean;
}

export interface TableInfo {
  name: string;
  estimated_rows: number;
  bytes: number;
}

export interface Info {
  version: string;
  started_at: string;
  uptime_secs: number;
  database_version: string;
  database_bytes: number;
  pool_size: number;
  pool_idle: number;
  config: Record<string, unknown>;
  counts: Record<string, number | null>;
  log_buffer: number;
  log_filter: string;
}

/** Developer mode is saved in the account (`users.preferences.developer`). Off by default. Owner and admin only. */
export function useDeveloperMode() {
  const { user, isAdmin } = useUser();
  const qc = useQueryClient();
  const on = isAdmin && (user?.preferences as { developer?: boolean } | undefined)?.developer === true;
  const set = useMutation({
    mutationFn: (developer: boolean) =>
      http.patch("/v1/users/me", { body: { preferences: { developer } } }),
    onSuccess: () => qc.invalidateQueries({ queryKey: authStateKey }),
    onError: (e) => toast.error(messageOf(e)),
  });
  return { on, isAdmin, set: (v: boolean) => set.mutate(v), saving: set.isPending };
}

export const tablesQuery = {
  queryKey: ["dev", "tables"],
  queryFn: () => http.get<{ tables: TableInfo[] }>("/v1/dev/tables"),
};

export function useInfo(enabled: boolean) {
  return useQuery({
    queryKey: ["dev", "info"],
    queryFn: () => http.get<Info>("/v1/dev/info"),
    refetchInterval: enabled ? 5000 : false,
    enabled,
  });
}

/** Copy text. Falls back to a hidden textarea when the browser has no clipboard API (plain HTTP). */
export async function copyText(text: string): Promise<void> {
  try {
    await navigator.clipboard.writeText(text);
    toast.success("Copied.");
    return;
  } catch {
    // try the old way
  }
  const area = document.createElement("textarea");
  area.value = text;
  area.style.position = "fixed";
  area.style.opacity = "0";
  document.body.appendChild(area);
  area.select();
  try {
    document.execCommand("copy");
    toast.success("Copied.");
  } catch {
    toast.error("The browser refused to copy. Select the text and copy it by hand.");
  } finally {
    area.remove();
  }
}

export function fmtBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 ** 3) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 ** 3).toFixed(2)} GB`;
}

export function fmtUptime(s: number): string {
  const d = Math.floor(s / 86400);
  const h = Math.floor((s % 86400) / 3600);
  const m = Math.floor((s % 3600) / 60);
  return d > 0 ? `${d}d ${h}h` : h > 0 ? `${h}h ${m}m` : `${m}m ${s % 60}s`;
}

export function logLine(e: LogEntry): string {
  const parts = [e.at, e.level.padEnd(5), e.target, e.message];
  if (e.fields) parts.push(e.fields);
  if (e.span) parts.push(`[${e.span}]`);
  return parts.join(" ");
}
