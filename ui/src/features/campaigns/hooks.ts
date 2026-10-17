import { queryOptions } from "@tanstack/react-query";
import { http, type S } from "@/api/http";

export const campaignQuery = (id: string) =>
  queryOptions({
    queryKey: ["campaigns", id],
    queryFn: () => http.get<S["CampaignDetail"]>("/v1/campaigns/{id}", { path: { id } }),
  });

export const statsQuery = (id: string) =>
  queryOptions({
    queryKey: ["campaigns", id, "stats"],
    queryFn: () => http.get<S["StatsReply"]>("/v1/campaigns/{id}/stats", { path: { id } }),
    staleTime: 30_000,
  });

export const sequenceQuery = (id: string) =>
  queryOptions({
    queryKey: ["sequences", id],
    queryFn: () => http.get<S["SequenceDetail"]>("/v1/followups/sequences/{id}", { path: { id } }),
  });

export function tone(status: S["CampaignStatus"]): "success" | "warning" | "neutral" | "info" {
  return status === "active"
    ? "success"
    : status === "paused"
      ? "warning"
      : status === "completed"
        ? "info"
        : "neutral";
}
