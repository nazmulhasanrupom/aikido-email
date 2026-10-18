import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { MoreHorizontal, Pencil } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import { MenuContent, MenuItem, MenuRoot, MenuTrigger } from "@/components/shell/menu";
import {
  Button,
  EmptyState,
  ErrorNote,
  Head,
  Modal,
  Pill,
  Select,
  Skeleton,
  Table,
  TD,
} from "@/components/ui";
import { campaignQuery } from "@/features/campaigns/hooks";
import { GenerateDialog, RunProgress } from "@/features/campaigns/AiTools";
import { EmailDrawer, StatusChip, stepName } from "@/features/campaigns/EmailDrawer";
import { ImportModal } from "@/features/leads/ImportModal";
import { AddColumn, ColumnHeader, ColumnsMenu, EditableCell, type GridCol, useColumns } from "@/features/leads/grid";
import { fmtDateTime } from "@/lib/format";
import { say } from "@/lib/words";

export const Route = createFileRoute("/w/$ws/campaigns/$id/leads")({ component: CampaignLeads });

const BASE_COLS: GridCol[] = [
  { id: "email", label: "Email" },
  { id: "first_name", label: "First name" },
  { id: "last_name", label: "Last name" },
  { id: "company_name", label: "Company" },
  { id: "state", label: "State" },
  { id: "follow", label: "Follow-up" },
  { id: "interest", label: "Interest" },
];

const STATES = ["pending", "enrolled", "skipped"] as const;

function followText(r: S["CampaignLeadRow"], ai: boolean): string {
  if (r.state === "pending") {
    const primary = r.emails.find((e) => e.step_number === 1);
    return ai && !(primary && (primary.status === "ready" || primary.status === "stale"))
      ? "Waiting for email"
      : "Waiting to start";
  }
  if (r.state === "skipped") return say("skip", r.skip_reason);
  if (r.enrollment_status === "stopped" || r.enrollment_status === "failed") return "Stopped";
  if (r.enrollment_status === "finished") return "Finished";
  if (r.enrollment_status === "paused") return "Paused";
  if (r.wait_reason) return say("wait", r.wait_reason);
  const base = `Step ${r.current_step ?? 1} of ${r.steps_total}`;
  return r.next_due_at ? `${base}, next ${fmtDateTime(r.next_due_at)}` : base;
}

function CampaignLeads() {
  const { id, ws } = Route.useParams();
  const qc = useQueryClient();
  const camp = useQuery(campaignQuery(id));
  const lists = useQuery(q.lists);
  const stepCols: GridCol[] = (camp.data?.steps ?? []).map((st) => ({
    id: `email:${st.step_number}`,
    label: stepName(st.step_number),
  }));
  const cols = useColumns("campaign_leads", [...BASE_COLS, ...stepCols]);
  const [genOpen, setGenOpen] = useState(false);
  const [openEmail, setOpenEmail] = useState<null | { lead: string; email: string; step: number }>(null);
  const [state, setState] = useState<string>("");
  const [onlyMissing, setOnlyMissing] = useState(false);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [menu, setMenu] = useState<null | "list" | "import">(null);
  const [listId, setListId] = useState("");
  const leads = useInfiniteQuery({
    queryKey: ["campaigns", id, "leads", state, onlyMissing],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      http.get<S["CampaignLeadsReply"]>("/v1/campaigns/{id}/leads", {
        path: { id },
        query: { state, limit: 50, cursor: pageParam, ...(onlyMissing ? { missing: true } : {}) },
      }),
    getNextPageParam: (l) => l.next_cursor ?? undefined,
    refetchInterval: 15_000,
  });
  const rows = leads.data?.pages.flatMap((p) => p.leads) ?? [];
  const counts = camp.data?.counts;
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["campaigns", id] });
  };
  const enrollAct = useMutation({
    mutationFn: ({ eid, what }: { eid: string; what: string }) =>
      http.post(`/v1/followups/enrollments/{id}/${what}`, { path: { id: eid }, body: {} }),
    onSuccess: refresh,
    onError: (e) => toast.error(messageOf(e)),
  });
  const pendingPicked = rows.filter((r) => picked.has(r.lead_id) && r.state === "pending").map((r) => r.lead_id);
  const needReview = rows.some((r) => r.emails.some((e) => e.status === "review"));
  const approveAll = useMutation({
    mutationFn: () =>
      http.post<S["ActionReply"]>("/v1/campaigns/{id}/emails/actions", {
        path: { id },
        body: { action: "approve", filter: { status: "review" } },
      }),
    onSuccess: (r) => {
      toast.success(`${r.changed} approved.`);
      refresh();
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const remove = useMutation({
    mutationFn: () =>
      http.del("/v1/campaigns/{id}/leads", { path: { id }, body: { lead_ids: pendingPicked } }),
    onSuccess: () => {
      setPicked(new Set());
      refresh();
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const addList = useMutation({
    mutationFn: () =>
      http.post<S["AddLeadsReply"]>("/v1/campaigns/{id}/leads", {
        path: { id },
        body: { list_id: listId },
      }),
    onSuccess: (r) => {
      toast.success(`${r.added} added.`);
      setMenu(null);
      refresh();
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const skipTip = counts
    ? Object.entries(counts.skipped_by_reason)
        .map(([k, v]) => `${say("skip", k)}: ${v}`)
        .join(", ")
    : "";
  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          onClick={() => setState("")}
          aria-pressed={state === ""}
          className={`rounded-full border px-3 py-1 text-sm ${state === "" ? "border-accent bg-accent-soft" : "border-border"}`}
        >
          All
        </button>
        {STATES.map((s) => (
          <button
            key={s}
            type="button"
            aria-pressed={state === s}
            title={s === "skipped" ? skipTip : undefined}
            onClick={() => setState(s)}
            className={`rounded-full border px-3 py-1 text-sm ${state === s ? "border-accent bg-accent-soft" : "border-border"}`}
          >
            {say("campaignLead", s)} {counts ? counts[s] : ""}
          </button>
        ))}
        <button
          type="button"
          aria-pressed={onlyMissing}
          onClick={() => setOnlyMissing(!onlyMissing)}
          className={`rounded-full border px-3 py-1 text-sm ${onlyMissing ? "border-accent bg-accent-soft" : "border-border"}`}
        >
          Missing a value used in your emails
        </button>
        <div className="ml-auto flex gap-2">
          {pendingPicked.length > 0 && (
            <Button variant="secondary" busy={remove.isPending} onClick={() => remove.mutate()}>
              Remove {pendingPicked.length} pending
            </Button>
          )}
          {needReview && (
            <Button variant="secondary" busy={approveAll.isPending} onClick={() => approveAll.mutate()}>
              Approve all that need review
            </Button>
          )}
          {camp.data?.content_mode === "ai" && (
            <Button variant="secondary" onClick={() => setGenOpen(true)}>
              Generate emails{picked.size > 0 ? ` for ${picked.size}` : ""}
            </Button>
          )}
          <ColumnsMenu cols={cols} />
          <Button variant="secondary" onClick={() => setMenu("list")}>
            Add from a list
          </Button>
          <Button variant="secondary" onClick={() => setMenu("import")}>
            Import CSV
          </Button>
          <a
            className="inline-flex h-9 items-center rounded-(--radius-control) border border-border px-3.5 text-sm font-medium hover:bg-surface-2"
            href={`/v1/leads/export.csv?campaign_id=${id}`}
            download
          >
            Export
          </a>
        </div>
      </div>
      <RunProgress campaignId={id} />
      {leads.isPending && <Skeleton />}
      {leads.isError && <ErrorNote error={leads.error} />}
      {leads.data && rows.length === 0 && (
        <EmptyState text="No leads here yet. Add leads from a list or import a CSV file." />
      )}
      {rows.length > 0 && (
        <Table>
          <Head
            cols={[
              "",
              ...cols.visible.map((c) => <ColumnHeader key={c.id} col={c} />),
              ...(onlyMissing ? ["Fill in"] : []),
              <AddColumn key="add" />,
            ]}
          />
          <tbody>
            {rows.map((r) => (
              <tr key={r.lead_id} className="border-t border-border">
                <td className={TD}>
                  <input
                    type="checkbox"
                    aria-label={`Select ${r.email}`}
                    checked={picked.has(r.lead_id)}
                    onChange={(e) => {
                      const n = new Set(picked);
                      e.target.checked ? n.add(r.lead_id) : n.delete(r.lead_id);
                      setPicked(n);
                    }}
                  />
                </td>
                {cols.visible.map((c) => (
                  <td key={c.id} className={TD}>
                    {c.id.startsWith("email:") ? (
                      <EmailCell
                        cell={r.emails.find((x) => `email:${x.step_number}` === c.id)}
                        onOpen={() =>
                          setOpenEmail({ lead: r.lead_id, email: r.email, step: Number(c.id.slice(6)) })
                        }
                      />
                    ) : c.id === "state" ? (
                      <Pill>{say("campaignLead", r.state)}</Pill>
                    ) : c.id === "follow" ? (
                      followText(r, camp.data?.content_mode === "ai")
                    ) : c.id === "interest" ? (
                      r.interest ? (
                        <Pill tone="accent">{say("interest", r.interest)}</Pill>
                      ) : (
                        ""
                      )
                    ) : (
                      <div className="flex items-center gap-1">
                        <EditableCell
                          lead={{ ...r, id: r.lead_id }}
                          col={c}
                          inCampaign
                          locked={
                            !!r.enrollment_id &&
                            ["active", "waiting_send", "paused"].includes(r.enrollment_status ?? "")
                          }
                        />
                        {c.id === "email" && (
                          <a
                            aria-label={`Open ${r.email}`}
                            title="Open lead"
                            className="rounded p-1 text-muted hover:bg-surface-2"
                            href={`/w/${ws}/leads?lead=${r.lead_id}`}
                          >
                            <Pencil size={13} />
                          </a>
                        )}
                      </div>
                    )}
                  </td>
                ))}
                {onlyMissing && (
                  <td className={TD}>
                    <div className="flex flex-col gap-1">
                      {r.missing.map((name) => {
                        const known = cols.all.find((c) => c.id === name || c.id === `custom:${name}`);
                        return (
                          <div key={name} className="flex items-center gap-2">
                            <span className="w-28 truncate text-xs text-muted">{name}</span>
                            <EditableCell lead={{ ...r, id: r.lead_id }} col={known ?? { id: name, label: name }} inCampaign />
                          </div>
                        );
                      })}
                    </div>
                  </td>
                )}
                <td className={TD}>
                  {r.enrollment_id && (
                    <MenuRoot>
                      <MenuTrigger
                        aria-label={`Actions for ${r.email}`}
                        className="rounded p-1 hover:bg-surface-2"
                      >
                        <MoreHorizontal size={16} />
                      </MenuTrigger>
                      <MenuContent align="end">
                        <MenuItem
                          onSelect={() =>
                            enrollAct.mutate({ eid: r.enrollment_id as string, what: "stop" })
                          }
                        >
                          Stop sequence
                        </MenuItem>
                        <MenuItem
                          onSelect={() =>
                            enrollAct.mutate({ eid: r.enrollment_id as string, what: "pause" })
                          }
                        >
                          Pause
                        </MenuItem>
                        <MenuItem
                          onSelect={() =>
                            enrollAct.mutate({ eid: r.enrollment_id as string, what: "resume" })
                          }
                        >
                          Resume
                        </MenuItem>
                      </MenuContent>
                    </MenuRoot>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </Table>
      )}
      {leads.hasNextPage && (
        <div>
          <Button variant="secondary" onClick={() => leads.fetchNextPage()}>
            Load more
          </Button>
        </div>
      )}
      <Modal
        open={menu === "list"}
        onClose={() => setMenu(null)}
        size="sm"
        title="Add leads from a list"
        footer={
          <>
            <Button variant="secondary" onClick={() => setMenu(null)}>
              Cancel
            </Button>
            <Button disabled={!listId} busy={addList.isPending} onClick={() => addList.mutate()}>
              Add leads
            </Button>
          </>
        }
      >
        <Select
          label="List"
          value={listId}
          onChange={setListId}
          options={[
            { value: "", label: "Choose..." },
            ...(lists.data ?? []).map((l) => ({
              value: l.id,
              label: `${l.name} (${l.member_count})`,
            })),
          ]}
        />
        <p className="mt-3 text-xs text-muted">
          To add leads by filter, use Leads, select them and choose Add to campaign.
        </p>
      </Modal>
      <ImportModal
        open={menu === "import"}
        onClose={() => {
          setMenu(null);
          refresh();
        }}
        campaignId={id}
      />
      {camp.data && (
        <GenerateDialog open={genOpen} onClose={() => setGenOpen(false)} campaign={camp.data} selected={[...picked]} />
      )}
      <EmailDrawer
        campaignId={id}
        leadId={openEmail?.lead ?? null}
        email={openEmail?.email ?? ""}
        step={openEmail?.step ?? 1}
        onClose={() => setOpenEmail(null)}
      />
    </div>
  );
}

function EmailCell({ cell, onOpen }: { cell: S["EmailCell"] | undefined; onOpen: () => void }) {
  return (
    <button type="button" onClick={onOpen} className="flex max-w-56 flex-col items-start gap-0.5 text-left">
      <StatusChip status={cell?.status ?? "none"} hint={cell?.error_code} />
      {cell && (
        <span className="max-w-full truncate text-xs text-muted">
          {cell.variant_key ? `${cell.variant_key}: ` : ""}
          {cell.subject_preview}
        </span>
      )}
    </button>
  );
}
