import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { http, type S } from "@/api/http";
import { Card, ErrorNote, Head, Skeleton, Table, TD } from "@/components/ui";
import { campaignQuery, statsQuery } from "@/features/campaigns/hooks";
import { ProgressBar, progressOf } from "@/features/campaigns/Progress";
import { VariantResults } from "@/features/campaigns/VariantResults";
import { opensQuery } from "@/features/tracking/api";
import { DailyTrend } from "@/features/reports/DailyChart";
import { RangePicker, StatCard, useRange } from "@/features/reports/parts";
import { fmtInt, fmtPct } from "@/lib/format";

export const Route = createFileRoute("/w/$ws/campaigns/$id/reports")({
  component: CampaignReports,
});

function CampaignReports() {
  const { id, ws } = Route.useParams();
  const [range, setRange] = useRange("30");
  const stats = useQuery(statsQuery(id));
  const opens = useQuery(opensQuery(id));
  const campaign = useQuery(campaignQuery(id));
  const usage = useQuery({
    queryKey: ["generator", "usage", range.from, range.to],
    queryFn: () =>
      http.get<S["UsageReply"]>("/v1/generator/usage", {
        query: { from: range.from, to: range.to },
      }),
  });
  if (stats.isPending) return <Skeleton />;
  if (stats.isError) return <ErrorNote error={stats.error} />;
  const s = stats.data;
  const progress =
    campaign.data && campaign.data.status !== "draft" ? progressOf(campaign.data.counts) : null;
  const mine = usage.data?.by_campaign.find((r) => r.key === id);
  return (
    <div className="flex flex-col gap-6">
      <div className="flex flex-wrap items-center gap-4">
        <RangePicker value={range} onChange={setRange} />
        {progress != null && <ProgressBar value={progress} />}
      </div>
      <p className="-mt-3 text-xs text-muted">
        The cards show the whole life of the campaign. The charts follow the range.
      </p>
      <div className="grid grid-cols-2 gap-3 md:grid-cols-4">
        <StatCard color="var(--accent)" label="Leads contacted" value={fmtInt(s.leads_contacted)} />
        <StatCard color="var(--info)" label="Sent" value={fmtInt(s.sent)} />
        <StatCard color="var(--success)" label="Replies" value={fmtInt(s.replied)} />
        <StatCard color="var(--success)" label="Reply rate" value={fmtPct(s.reply_rate)} />
        <StatCard
          color="var(--danger)"
          label="Bounces"
          value={fmtInt(s.bounced)}
          sub={s.sent ? fmtPct(s.bounced / s.sent) : undefined}
        />
        <StatCard color="var(--warning)" label="Unsubscribes" value={fmtInt(s.unsubscribed)} />
        <StatCard color="var(--danger)" label="Spam complaints" value={fmtInt(s.complained)} />
        <StatCard color="var(--ai)" label="Out of office" value={fmtInt(s.auto_replied)} />
        {opens.data && opens.data.tracked_leads > 0 && (
          <>
            <StatCard color="var(--info)" label="Opened" value={fmtInt(opens.data.opened)} />
            <StatCard
              color="var(--warning)"
              label="Maybe opened"
              value={fmtInt(opens.data.maybe_opened)}
              sub="Only Apple, Google proxy or scanner opens"
            />
            <StatCard
              color="var(--success)"
              label="Open rate"
              value={fmtPct(opens.data.open_rate)}
              sub={`${fmtInt(opens.data.repeat_opens)} repeat opens`}
            />
          </>
        )}
      </div>
      <Card>
        <h2 className="font-semibold">Performance trend</h2>
        <p className="mb-5 text-sm text-muted">Campaign numbers per day in the range.</p>
        <DailyTrend
          range={range}
          extra={{ campaign_id: id }}
          keys={["sent", "replied", "bounced", "new_leads_enrolled"]}
        />
      </Card>
      <VariantResults campaignId={id} stats={s} />
      <div className="grid gap-5 lg:grid-cols-2">
        <section>
          <h2 className="mb-2 font-semibold">By step</h2>
          <Table>
            <Head cols={["Step", "Sent", "Replies", "Reply rate"]} />
            <tbody>
              {s.by_step.map((x) => (
                <tr key={x.step_number} className="border-t border-border">
                  <td className={TD}>
                    {x.step_number === 1 ? "Email 1" : `Follow-up ${x.step_number - 1}`}
                  </td>
                  <td className={TD}>{x.sent}</td>
                  <td className={TD}>{x.replied}</td>
                  <td className={TD}>{x.sent ? fmtPct(x.replied / x.sent) : "0%"}</td>
                </tr>
              ))}
            </tbody>
          </Table>
        </section>
        <section>
          <h2 className="mb-2 font-semibold">By account</h2>
          <Table>
            <Head cols={["Account", "Sent", "Replies", "Bounced"]} />
            <tbody>
              {s.by_account.map((x) => (
                <tr key={x.account_id} className="border-t border-border">
                  <td className={TD}>{x.email}</td>
                  <td className={TD}>{x.sent}</td>
                  <td className={TD}>{x.replied}</td>
                  <td className={TD}>{x.bounced}</td>
                </tr>
              ))}
            </tbody>
          </Table>
        </section>
      </div>
      <Card className="flex flex-col gap-1">
        <h2 className="font-semibold">AI usage</h2>
        {mine ? (
          <p className="text-muted">
            {fmtInt(mine.input_tokens + mine.output_tokens)} tokens in {fmtInt(mine.calls)} calls
            {mine.cost_usd != null ? `, about $${mine.cost_usd.toFixed(2)}` : ""}.
          </p>
        ) : (
          <p className="text-muted">No AI calls in this range.</p>
        )}
      </Card>
      <Link
        to="/w/$ws/inbox"
        params={{ ws }}
        search={{ campaign_id: id }}
        className="text-accent underline"
      >
        See replies in Inbox
      </Link>
    </div>
  );
}
