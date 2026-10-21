import { queryOptions } from "@tanstack/react-query";
import { http } from "@/api/http";

export type DomainStatus = "not_connected" | "checking" | "connected" | "error";

export interface DomainView {
  id: string;
  sending_domain: string;
  subdomain: string;
  host: string;
  status: DomainStatus;
  record: { type: string; name: string; value: string | null };
  last_checked_at: string | null;
  error_message: string | null;
  cert_expires_at: string | null;
  not_found: boolean;
}

export interface ManualStyle {
  font: string;
  size: number;
  color: string;
  bold: boolean;
  italic: boolean;
}

export type TrackerType = "text_image" | "signature_image";
export type Mode = "auto" | "manual";

export interface TrackingSettings {
  campaign_id: string;
  enabled: boolean;
  tracker_type: TrackerType | null;
  text_value: string | null;
  style_mode: Mode;
  manual_style: ManualStyle | null;
  image_mode: Mode;
  image_source_url: string | null;
  warning: string | null;
  warning_at: string | null;
  domains: DomainView[];
  trackers_unlocked: boolean;
}

/** What is saved. Every field is sent, the server replaces all settings. */
export type TrackingBody = Pick<
  TrackingSettings,
  | "enabled"
  | "tracker_type"
  | "text_value"
  | "style_mode"
  | "manual_style"
  | "image_mode"
  | "image_source_url"
>;

export interface Preview {
  tracker_type: TrackerType | null;
  image: string | null;
  width: number | null;
  height: number | null;
  style_source: "manual" | "email" | "default" | null;
  text_found: boolean | null;
  source_url: string | null;
  found: boolean | null;
  message: string | null;
}

export interface CampaignOpens {
  tracked_leads: number;
  opened: number;
  maybe_opened: number;
  open_rate: number;
  repeat_opens: number;
  total_opens: number;
  proxy_opens: number;
}

export const trackingQuery = (id: string) =>
  queryOptions({
    queryKey: ["tracking", "campaign", id],
    queryFn: () => http.get<TrackingSettings>("/v1/campaigns/{id}/tracking", { path: { id } }),
    // a domain that waits for its certificate turns green by itself
    refetchInterval: (q) =>
      q.state.data?.domains.some((d) => d.status === "checking") ? 10_000 : false,
  });

export const opensQuery = (id: string) =>
  queryOptions({
    queryKey: ["tracking", "opens", id],
    queryFn: () => http.get<CampaignOpens>("/v1/campaigns/{id}/opens", { path: { id } }),
  });

export const STATUS_WORDS: Record<DomainStatus, string> = {
  not_connected: "Not connected",
  checking: "Checking",
  connected: "Connected",
  error: "Error",
};

export const STATUS_TONE: Record<DomainStatus, "danger" | "warning" | "success"> = {
  not_connected: "danger",
  checking: "warning",
  connected: "success",
  error: "danger",
};
