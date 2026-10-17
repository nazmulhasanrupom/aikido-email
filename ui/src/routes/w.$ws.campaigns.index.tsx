import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { Send } from "lucide-react";
import { useState } from "react";
import { q } from "@/api/queries";
import {
  Button,
  EmptyState,
  ErrorNote,
  Head,
  Page,
  PageHeader,
  Pill,
  Select,
  Skeleton,
  Table,
  TD,
} from "@/components/ui";
import { statsQuery, tone } from "@/features/campaigns/hooks";
import { NewCampaign } from "@/features/campaigns/NewCampaign";
import { fmtDate, fmtInt, fmtPct } from "@/lib/format";
import { say } from "@/lib/words";

export const Route = createFileRoute("/w/$ws/campaigns/")({ component: Campaigns });

function Campaigns() {
  const { ws } = Route.useParams();
  const list = useQuery(q.campaigns);
  const [open, setOpen] = useState(false);
  const [filter, setFilter] = useState("");
  const rows = (list.data?.campaigns ?? []).filter((c) => !filter || c.status === filter);
  return (
    <Page wide>
      <PageHeader
        title="Campaigns"
        actions={<Button onClick={() => setOpen(true)}>New campaign</Button>}
      />
      {list.isPending && <Skeleton />}
      {list.isError && <ErrorNote error={list.error} onRetry={() => list.refetch()} />}
      {list.data?.campaigns.length === 0 && (
        <EmptyState
          icon={Send}
          text="Campaigns send a set of emails to your leads, one by one."
          action={<Button onClick={() => setOpen(true)}>New campaign</Button>}
        />
      )}
      {list.data && list.data.campaigns.length > 0 && (
        <>
          <div className="mb-3 w-48">
            <Select
              label="Status"
              value={filter}
              onChange={setFilter}
              options={[
                { value: "", label: "All" },
                ...["draft", "active", "paused", "completed", "archived"].map((s) => ({
                  value: s,
                  label: say("campaign", s),
                })),
              ]}
            />
          </div>
          <Table>
            <Head cols={["Name", "Status", "Leads", "Sent", "Reply rate", "Created"]} />
            <tbody>
              {rows.map((c, i) => (
                <Row key={c.id} ws={ws} c={c} visible={i < 25} />
              ))}
            </tbody>
          </Table>
        </>
      )}
      <NewCampaign open={open} onClose={() => setOpen(false)} ws={ws} />
    </Page>
  );
}

function Row({
  ws,
  c,
  visible,
}: {
  ws: string;
  c: import("@/api/http").S["CampaignSummary"];
  visible: boolean;
}) {
  const stats = useQuery({ ...statsQuery(c.id), enabled: visible && c.status !== "draft" });
  const total = c.counts.enrolled + c.counts.pending + c.counts.skipped;
  return (
    <tr className="border-t border-border hover:bg-surface-2/50">
      <td className={TD}>
        <Link
          to="/w/$ws/campaigns/$id"
          params={{ ws, id: c.id }}
          className="font-medium hover:underline"
        >
          {c.name}
        </Link>
      </td>
      <td className={TD}>
        <Pill tone={tone(c.status)}>{say("campaign", c.status)}</Pill>
      </td>
      <td className={TD}>
        {fmtInt(c.counts.enrolled)} / {fmtInt(total)}
      </td>
      <td className={TD}>{stats.data ? fmtInt(stats.data.sent) : ""}</td>
      <td className={TD}>{stats.data ? fmtPct(stats.data.reply_rate) : ""}</td>
      <td className={TD}>{fmtDate(c.created_at)}</td>
    </tr>
  );
}
