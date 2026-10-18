import { useQuery } from "@tanstack/react-query";
import { http, type S } from "@/api/http";
import { ErrorNote, Skeleton } from "@/components/ui";
import { AreaChart, LineChart, type Range } from "./parts";

export function dailyQuery(r: Range, extra?: Record<string, string>) {
  return {
    queryKey: ["reports", "daily", r.from, r.to, extra],
    queryFn: () =>
      http.get<S["DailyReply"]>("/v1/reports/daily", {
        query: { from: r.from, to: r.to, ...extra },
      }),
  };
}

type Key = "sent" | "replied" | "bounced" | "new_leads_enrolled";

const NAMES: Record<Key, string> = {
  sent: "Sent",
  replied: "Replies",
  bounced: "Bounces",
  new_leads_enrolled: "New leads",
};
const COLORS: Record<Key, string> = {
  sent: "var(--info)",
  replied: "var(--success)",
  bounced: "var(--danger)",
  new_leads_enrolled: "var(--accent)",
};

/** One small area chart per number, side by side, each with its total for the range. */
export function DailyTrend({
  range,
  extra,
  keys,
}: {
  range: Range;
  extra?: Record<string, string>;
  keys: Key[];
}) {
  const d = useQuery(dailyQuery(range, extra));
  if (d.isPending) return <Skeleton rows={4} />;
  if (d.isError) return <ErrorNote error={d.error} />;
  const labels = d.data.days.map((x) => x.date.slice(5));
  return (
    <div className="grid gap-x-10 gap-y-8 md:grid-cols-2">
      {keys.map((k) => (
        <div key={k}>
          <p className="mb-2 flex items-center gap-2 text-sm">
            <span
              aria-hidden="true"
              className="size-2 rounded-full"
              style={{ background: COLORS[k] }}
            />
            <span className="text-muted">{NAMES[k]}</span>
            <span className="font-semibold tabular-nums">{d.data.totals[k].toLocaleString()}</span>
          </p>
          <AreaChart
            labels={labels}
            series={{ name: NAMES[k], color: COLORS[k], values: d.data.days.map((x) => x[k]) }}
          />
        </div>
      ))}
    </div>
  );
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
