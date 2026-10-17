import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import {
  Card,
  ErrorNote,
  Head,
  Page,
  PageHeader,
  Pill,
  Skeleton,
  Table,
  TD,
} from "@/components/ui";
import { statsQuery, tone } from "@/features/campaigns/hooks";
import { DailyChart, dailyQuery } from "@/features/reports/DailyChart";
import { LineChart, RangePicker, StatCard, useRange } from "@/features/reports/parts";
import { fmtInt, fmtPct } from "@/lib/format";
import { say } from "@/lib/words";

export const Route = createFileRoute("/w/$ws/reports")({ component: Reports });

function Reports() {
  const [range, setRange] = useRange("30");
  const daily = useQuery(dailyQuery(range));
  const accounts = useQuery({
    queryKey: ["reports", "accounts", range.from, range.to],
    queryFn: () =>
      http.get<S["AccountsReply"]>("/v1/reports/accounts", {
        query: { from: range.from, to: range.to },
      }),
  });
  const campaigns = useQuery(q.campaigns);
  const usage = useQuery({
    queryKey: ["generator", "usage", range.from, range.to],
    queryFn: () =>
      http.get<S["UsageReply"]>("/v1/generator/usage", {
        query: { from: range.from, to: range.to },
      }),
  });
  const t = daily.data?.totals;
  const bounceRate = t && t.sent > 0 ? t.bounced / t.sent : 0;
  const replyRate = t && t.sent > 0 ? t.replied / t.sent : 0;
  return (
    <Page wide>
      <PageHeader
        title="Reports"
        sub={daily.data ? `Days are counted in ${daily.data.tz}.` : undefined}
      />
      <div className="mb-5">
        <RangePicker value={range} onChange={setRange} />
      </div>
      {daily.isError && <ErrorNote error={daily.error} />}
      {daily.isPending ? (
        <Skeleton rows={3} />
      ) : (
        t && (
          <div className="mb-6 grid grid-cols-2 gap-3 md:grid-cols-4 xl:grid-cols-7">
            <StatCard label="Sent" value={fmtInt(t.sent)} />
            <StatCard label="Replies" value={fmtInt(t.replied)} />
            <StatCard label="Reply rate" value={fmtPct(replyRate)} />
            <StatCard label="Bounce rate" value={fmtPct(bounceRate)} />
            <StatCard label="Unsubscribes" value={fmtInt(t.unsubscribed)} />
            <StatCard label="Spam complaints" value={fmtInt(t.complained)} />
            <StatCard label="Leads added" value={fmtInt(t.new_leads_enrolled)} />
          </div>
        )
      )}
      <Card className="mb-6">
        <h2 className="mb-2 font-semibold">Per day</h2>
        <DailyChart range={range} keys={["sent", "replied", "bounced"]} />
      </Card>
      <div className="mb-6 grid gap-5 lg:grid-cols-2">
        <section>
          <h2 className="mb-2 font-semibold">Campaigns</h2>
          <Table>
            <Head cols={["Name", "Status", "Sent", "Replies", "Reply rate", "Bounces"]} />
            <tbody>
              {(campaigns.data?.campaigns ?? [])
                .filter((c) => c.status !== "draft")
                .map((c) => (
                  <CampaignRow key={c.id} c={c} />
                ))}
            </tbody>
          </Table>
        </section>
        <section>
          <h2 className="mb-2 font-semibold">Accounts</h2>
          <Table>
            <Head cols={["Email", "Sent", "Reply rate", "Bounce rate", "Status"]} />
            <tbody>
              {(accounts.data?.accounts ?? []).map((a) => {
                const near = a.bounce_rate * 100 >= (accounts.data?.bounce_limit_pct ?? 5) * 0.6;
                return (
                  <tr key={a.account_id} className="border-t border-border">
                    <td className={TD}>{a.email}</td>
                    <td className={TD}>{fmtInt(a.sent)}</td>
                    <td className={TD}>{fmtPct(a.reply_rate)}</td>
                    <td className={`${TD} ${near ? "font-medium text-warning" : ""}`}>
                      {fmtPct(a.bounce_rate)}
                    </td>
                    <td className={TD}>
                      <Pill tone={a.status === "active" ? "success" : "warning"}>
                        {say("account", a.status)}
                      </Pill>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </Table>
        </section>
      </div>
      <Card className="flex flex-col gap-2">
        <h2 className="font-semibold">AI</h2>
        {usage.data && usage.data.by_day.length > 0 ? (
          <>
            <p className="text-muted">
              {fmtInt(usage.data.total.input_tokens + usage.data.total.output_tokens)} tokens in{" "}
              {fmtInt(usage.data.total.calls)} calls
              {usage.data.total.cost_usd != null
                ? `, about $${usage.data.total.cost_usd.toFixed(2)}`
                : ""}
              .
            </p>
            <LineChart
              height={140}
              labels={usage.data.by_day.map((d) => d.key.slice(5))}
              series={[
                {
                  name: "Tokens",
                  color: "var(--ai)",
                  values: usage.data.by_day.map((d) => d.input_tokens + d.output_tokens),
                },
              ]}
            />
          </>
        ) : (
          <p className="text-muted">No AI calls in this range.</p>
        )}
      </Card>
    </Page>
  );
}

function CampaignRow({ c }: { c: S["CampaignSummary"] }) {
  const s = useQuery(statsQuery(c.id));
  return (
    <tr className="border-t border-border">
      <td className={`${TD} font-medium`}>{c.name}</td>
      <td className={TD}>
        <Pill tone={tone(c.status)}>{say("campaign", c.status)}</Pill>
      </td>
      <td className={TD}>{s.data ? fmtInt(s.data.sent) : ""}</td>
      <td className={TD}>{s.data ? fmtInt(s.data.replied) : ""}</td>
      <td className={TD}>{s.data ? fmtPct(s.data.reply_rate) : ""}</td>
      <td className={TD}>{s.data ? fmtInt(s.data.bounced) : ""}</td>
    </tr>
  );
}
