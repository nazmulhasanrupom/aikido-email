import { queryOptions, useQuery } from "@tanstack/react-query";
import { http } from "@/api/http";

export interface AppInfo {
  app: string;
  name: string;
  description: string;
  sidebar: boolean;
  installed: boolean;
  status: "installed" | "auth_failed" | "disabled" | null;
}

export interface SendRule {
  allowed: string[];
  allow_not_verified: boolean;
}

export interface ReoonSettings {
  mode: "power" | "quick";
  concurrency: number;
  reuse_days: number;
  tick_secs: number;
  send_rule: SendRule;
  auto_verify: boolean;
}

export interface Balance {
  daily_credits: number | null;
  instant_credits: number | null;
}

export interface Result {
  status: string;
  mode: string;
  score: number | null;
  safe_to_send: boolean | null;
  flags: Record<string, boolean>;
  checked_at: string;
}

export interface VerifyRow {
  id: string;
  email: string;
  first_name: string | null;
  last_name: string | null;
  company_name: string | null;
  result: Result | null;
}

export interface Run {
  id: string;
  mode: string;
  status: "queued" | "running" | "done" | "cancelled" | "failed";
  force: boolean;
  auto: boolean;
  total: number;
  done_count: number;
  failed_count: number;
  skipped_count: number;
  credits_used: number;
  warning: string | null;
  error: string | null;
  created_at: string;
  finished_at: string | null;
}

export const appsQuery = queryOptions({
  queryKey: ["integrations", "apps"],
  queryFn: async () => (await http.get<{ apps: AppInfo[] }>("/v1/integrations/apps")).apps,
  staleTime: 30_000,
});

export const reoonSettingsQuery = queryOptions({
  queryKey: ["reoon", "settings"],
  queryFn: () => http.get<{ settings: ReoonSettings; engine_paused: boolean }>("/v1/verify/reoon/settings"),
});

/** Is Reoon installed, and does its key work? */
export function useReoon() {
  const apps = useQuery(appsQuery);
  const app = apps.data?.find((a) => a.app === "reoon");
  return { installed: !!app?.installed, status: app?.status ?? null, app, loading: apps.isPending };
}

/** Every status a send rule can name, in the order of the dialog. `locked` ones can never be allowed. */
export const STATUSES: { code: string; locked?: string }[] = [
  { code: "safe" },
  { code: "valid" },
  { code: "catch_all" },
  { code: "role_account" },
  { code: "unknown" },
  { code: "inbox_full" },
  { code: "invalid", locked: "The address does not exist." },
  { code: "disabled", locked: "The mailbox is turned off." },
  { code: "disposable", locked: "A throwaway address." },
  { code: "spamtrap", locked: "A trap that hurts your sender reputation." },
];

export function statusTone(status: string): "success" | "warning" | "danger" | "neutral" {
  if (status === "safe" || status === "valid") return "success";
  if (["catch_all", "role_account", "unknown", "inbox_full"].includes(status)) return "warning";
  if (["invalid", "disabled", "disposable", "spamtrap"].includes(status)) return "danger";
  return "neutral";
}

export const FLAG_WORDS: Record<string, [string, string]> = {
  is_deliverable: ["Can receive mail", "Can not receive mail"],
  is_catch_all: ["The domain accepts all addresses", ""],
  is_disposable: ["Throwaway address", ""],
  is_role_account: ["Role address (info@, sales@)", ""],
  is_spamtrap: ["Spam trap", ""],
  has_inbox_full: ["Inbox is full", ""],
  is_disabled: ["Mailbox is disabled", ""],
  is_free_email: ["Free email provider", ""],
  can_connect_smtp: ["Mail server answered", "Mail server did not answer"],
  mx_accepts_mail: ["Domain accepts mail", "Domain does not accept mail"],
};
