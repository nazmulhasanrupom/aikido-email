import { queryOptions } from "@tanstack/react-query";
import { http, type S } from "./http";

/** The answer of `GET /v1/sending/accounts/{id}/status` (the generated type for it is wrong). */
export interface SendStatus {
  account_id: string;
  email: string;
  account_status: S["AccountStatus"];
  paused: boolean;
  paused_reason?: string | null;
  paused_at?: string | null;
  timezone: string;
  local_time: string;
  inside_window: boolean;
  sent_today: number;
  today_limit: number;
  warmup_day: number;
  consecutive_failures: number;
  next_send_at?: string | null;
  last_sent_at?: string | null;
  queued_jobs: number;
  sending_jobs: number;
  wait?: S["WaitInfo"] | null;
}

/** A model provider as the generator routes give it. */
export interface GenProvider {
  id: string;
  kind: string;
  name: string;
  model: string;
  base_url: string;
  json_mode?: boolean;
  has_api_key?: boolean;
  status: string;
  max_concurrency?: number;
  timeout_secs?: number;
}

export const q = {
  config: queryOptions({
    queryKey: ["ui", "config"],
    queryFn: () => http.get<S["UiConfig"]>("/v1/ui/config"),
    staleTime: 60_000,
  }),
  onboarding: queryOptions({
    queryKey: ["ui", "onboarding"],
    queryFn: () => http.get<S["OnboardingReply"]>("/v1/ui/onboarding"),
  }),
  alerts: queryOptions({
    queryKey: ["ui", "alerts"],
    queryFn: () => http.get<S["Alert"][]>("/v1/ui/alerts"),
    refetchInterval: 60_000,
  }),
  accounts: queryOptions({
    queryKey: ["accounts"],
    queryFn: () => http.get<S["AccountView"][]>("/v1/accounts"),
  }),
  sendStatus: (id: string) =>
    queryOptions({
      queryKey: ["accounts", id, "status"],
      queryFn: () => http.get<SendStatus>("/v1/sending/accounts/{id}/status", { path: { id } }),
      refetchInterval: 30_000,
    }),
  providers: queryOptions({
    queryKey: ["generator", "providers"],
    queryFn: async () =>
      (await http.get<{ providers: GenProvider[] }>("/v1/generator/providers")).providers,
  }),
  genSettings: queryOptions({
    queryKey: ["generator", "settings"],
    queryFn: () => http.get<{ settings: S["GeneratorSettings"] }>("/v1/generator/settings"),
  }),
  leadSettings: queryOptions({
    queryKey: ["leads", "settings"],
    queryFn: () => http.get<{ settings: S["LeadSettings"] }>("/v1/leads/settings"),
  }),
  fields: queryOptions({
    queryKey: ["leads", "fields"],
    queryFn: () => http.get<S["LeadField"][]>("/v1/leads/fields"),
  }),
  lists: queryOptions({
    queryKey: ["leads", "lists"],
    queryFn: () => http.get<S["LeadList"][]>("/v1/leads/lists"),
  }),
  campaigns: queryOptions({
    queryKey: ["campaigns"],
    queryFn: () => http.get<S["ListCampaignsReply"]>("/v1/campaigns", { query: { limit: 200 } }),
  }),
  sendingDefaults: queryOptions({
    queryKey: ["sending", "defaults"],
    queryFn: () => http.get<S["SettingsReply"]>("/v1/sending/defaults"),
  }),
};
