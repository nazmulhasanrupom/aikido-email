import { useQuery } from "@tanstack/react-query";
import { http, type S } from "@/api/http";
import { ErrorNote, Skeleton } from "@/components/ui";
import { LineChart, type Range } from "./parts";

export function dailyQuery(r: Range, extra?: Record<string, string>) {
  return {
    queryKey: ["reports", "daily", r.from, r.to, extra],
    queryFn: () =>
      http.get<S["DailyReply"]>("/v1/reports/daily", {
        query: { from: r.from, to: r.to, ...extra },
      }),
  };
}

export function DailyChart({
  range,
  extra,
  keys,
}: {
  range: Range;
  extra?: Record<string, string>;
  keys: ("sent" | "replied" | "bounced" | "new_leads_enrolled")[];
}) {
  const d = useQuery(dailyQuery(range, extra));
  if (d.isPending) return <Skeleton rows={4} />;
  if (d.isError) return <ErrorNote error={d.error} />;
  const names = {
    sent: "Sent",
    replied: "Replies",
    bounced: "Bounces",
    new_leads_enrolled: "New leads",
  };
  const colors = {
    sent: "var(--info)",
    replied: "var(--success)",
    bounced: "var(--danger)",
    new_leads_enrolled: "var(--accent)",
  };
  return (
    <LineChart
      labels={d.data.days.map((x) => x.date.slice(5))}
      series={keys.map((k) => ({
        name: names[k],
        color: colors[k],
        values: d.data.days.map((x) => x[k]),
      }))}
    />
  );
}
