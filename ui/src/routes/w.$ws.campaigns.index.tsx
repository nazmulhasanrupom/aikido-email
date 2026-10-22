import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { ListFilter, Plus, Send } from "lucide-react";
import { useState } from "react";
import { q } from "@/api/queries";
import { Button, EmptyState, ErrorNote, Page, PageHeader, Pill, Skeleton } from "@/components/ui";
import { statsQuery, tone } from "@/features/campaigns/hooks";
import { GetStarted } from "@/features/home/GetStarted";
import { NewCampaign } from "@/features/campaigns/NewCampaign";
import { ProgressBar, progressOf } from "@/features/campaigns/Progress";
import { fmtDate, fmtInt, fmtPct } from "@/lib/format";
import { say } from "@/lib/words";

export const Route = createFileRoute("/w/$ws/campaigns/")({ component: Campaigns });

const STATUSES = ["draft", "active", "paused", "completed", "archived"] as const;
const HEAD = "px-4 py-3 text-left text-[11px] font-semibold uppercase tracking-wider text-muted";

function Campaigns() {
  const { ws } = Route.useParams();
  const list = useQuery(q.campaigns);
  const [open, setOpen] = useState(false);
  const [filter, setFilter] = useState("");
  const rows = (list.data?.campaigns ?? []).filter((c) => !filter || c.status === filter);
  const newButton = (
    <Button onClick={() => setOpen(true)}>
      <Plus size={16} aria-hidden="true" /> New campaign
    </Button>
  );
  return (
    <Page wide>
      <PageHeader title="Campaigns" actions={newButton} />
      <GetStarted ws={ws} />
      {list.isPending && <Skeleton />}
      {list.isError && <ErrorNote error={list.error} onRetry={() => list.refetch()} />}
      {list.data?.campaigns.length === 0 && (
        <EmptyState
          icon={Send}
          text="Campaigns send a set of emails to your leads, one by one."
          action={newButton}
        />
      )}
      {list.data && list.data.campaigns.length > 0 && (
        <>
          <div className="mb-3 flex items-center gap-2">
            <label className="relative inline-flex items-center">
              <span className="sr-only">Status</span>
              <ListFilter
                size={15}
                aria-hidden="true"
                className="pointer-events-none absolute left-3 text-muted"
              />
              <select
                value={filter}
                onChange={(e) => setFilter(e.target.value)}
                className="h-9 rounded-(--radius-control) border border-border bg-surface pr-3 pl-9 text-sm hover:bg-surface-2"
              >
                <option value="">All statuses</option>
                {STATUSES.map((s) => (
                  <option key={s} value={s}>
                    {say("campaign", s)}
                  </option>
                ))}
              </select>
            </label>
            <span className="text-xs text-muted">
              {rows.length} of {list.data.campaigns.length}
            </span>
          </div>
          <div className="overflow-x-auto rounded-(--radius-card) border border-border bg-surface">
            <table className="w-full border-collapse text-sm">
              <thead>
                <tr className="border-b border-border">
                  {["Name", "Status", "Progress", "Activity", "Outcome", "Created"].map((h) => (
                    <th key={h} scope="col" className={HEAD}>
                      {h}
                    </th>
                  ))}
                </tr>
              </thead>
              <tbody>
                {rows.map((c, i) => (
                  <Row key={c.id} ws={ws} c={c} visible={i < 25} />
                ))}
                {rows.length === 0 && (
                  <tr>
                    <td colSpan={6} className="px-4 py-8 text-center text-muted">
                      No campaigns with this status.
                    </td>
                  </tr>
                )}
              </tbody>
            </table>
          </div>
        </>
      )}
      <NewCampaign open={open} onClose={() => setOpen(false)} ws={ws} />
    </Page>
  );
}

const CELL = "px-4 py-3.5 align-middle";

function Row({
  ws,
  c,
  visible,
}: {
  ws: string;
  c: import("@/api/http").S["CampaignSummary"];
  visible: boolean;
}) {
  const navigate = useNavigate();
  const stats = useQuery({ ...statsQuery(c.id), enabled: visible && c.status !== "draft" });
  const progress = c.status === "draft" ? null : progressOf(c.counts);
  const s = stats.data;
  return (
    <tr
      onClick={() => navigate({ to: "/w/$ws/campaigns/$id", params: { ws, id: c.id } })}
      className="cursor-pointer border-t border-border first:border-t-0 hover:bg-surface-2/60"
    >
      <td className={CELL}>
        <Link
          to="/w/$ws/campaigns/$id"
          params={{ ws, id: c.id }}
          onClick={(e) => e.stopPropagation()}
          className="font-medium hover:underline"
        >
          {c.name}
        </Link>
        <div className="mt-0.5 text-xs text-muted">
          {fmtInt(c.counts.enrolled)} of{" "}
          {fmtInt(c.counts.enrolled + c.counts.pending + c.counts.skipped)} leads in sequence
        </div>
      </td>
      <td className={CELL}>
        <Pill dot tone={tone(c.status)}>
          {say("campaign", c.status)}
        </Pill>
      </td>
      <td className={CELL}>
        {progress == null ? (
          <span className="text-muted">-</span>
        ) : (
          <ProgressBar value={progress} />
        )}
      </td>
      <td className={CELL}>
        <span className="text-muted">Sent </span>
        <span className="font-semibold tabular-nums">{s ? fmtInt(s.sent) : "0"}</span>
      </td>
      <td className={CELL}>
        <span className="text-muted">Replies </span>
        <span className="font-semibold tabular-nums">{s ? fmtInt(s.replied) : "0"}</span>
        {s && s.sent > 0 && (
          <span className="ml-1.5 text-xs text-muted">{fmtPct(s.reply_rate)}</span>
        )}
      </td>
      <td className={`${CELL} whitespace-nowrap text-muted`}>{fmtDate(c.created_at)}</td>
    </tr>
  );
}
