import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { ChevronDown, ListFilter, Maximize2, MoreHorizontal } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import {
  MenuContent,
  MenuItem,
  MenuRadioGroup,
  MenuRadioItem,
  MenuRoot,
  MenuSeparator,
  MenuTrigger,
} from "@/components/shell/menu";
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
import { GenerateDialog, RunProgress } from "@/features/campaigns/AiTools";
import { EmailDrawer, StatusChip, stepName } from "@/features/campaigns/EmailDrawer";
import { campaignQuery } from "@/features/campaigns/hooks";
import {
  AddColumn,
  ColumnHeader,
  EditableCell,
  type GridCol,
  useColumns,
} from "@/features/leads/grid";
import { ListToCampaign } from "@/features/leads/ImportFlow";
import { ImportModal } from "@/features/leads/ImportModal";
import { ManualLeadModal } from "@/features/leads/ManualLead";
import { STATUSES, useReoon } from "@/features/reoon/api";
import { VerifyPill } from "@/features/reoon/parts";
import { OpenPill } from "@/features/tracking/parts";
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
  { id: "opened", label: "Opened" },
];

const STATES = ["pending", "enrolled", "skipped"] as const;

/** One filter at a time: a state, the leads with a missing value, or the leads the verification rule skipped. */
type Filter = "" | (typeof STATES)[number] | "missing" | "rule";

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
  const reoon = useReoon();
  const cols = useColumns("campaign_leads", [
    ...BASE_COLS,
    ...(reoon.installed ? [{ id: "reoon", label: "Verification" }] : []),
    ...stepCols,
  ]);
  const [genOpen, setGenOpen] = useState(false);
  const [openEmail, setOpenEmail] = useState<null | { lead: string; email: string; step: number }>(
    null,
  );
  const [filter, setFilter] = useState<Filter>("");
  const [vstatus, setVstatus] = useState("");
  const state = (STATES as readonly string[]).includes(filter) ? filter : "";
  const onlyMissing = filter === "missing";
  const byRule = reoon.installed && filter === "rule";
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [menu, setMenu] = useState<null | "list" | "import" | "manual">(null);
  const [listId, setListId] = useState("");
  const pickedList = lists.data?.find((l) => l.id === listId);
  const leads = useInfiniteQuery({
    queryKey: ["campaigns", id, "leads", state, onlyMissing, vstatus, byRule],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      http.get<S["CampaignLeadsReply"]>("/v1/campaigns/{id}/leads", {
        path: { id },
        query: {
          state,
          limit: 50,
          cursor: pageParam,
          verification_status: vstatus,
          ...(byRule ? { state: "skipped", skip_reason: "verification_not_allowed" } : {}),
          ...(onlyMissing ? { missing: true } : {}),
        },
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
  // "Send now": the next email of the lead goes out at once, with no rules and no question.
  const sendNow = useMutation({
    mutationFn: (leadId: string) =>
      http.post<{ status: "sent" | "failed" | "queued"; step_number: number | null; error: string | null }>(
        "/v1/campaigns/{id}/leads/{lead_id}/send-now",
        { path: { id, lead_id: leadId } },
      ),
    onSuccess: (r) => {
      if (r.status === "sent") toast.success("Email sent.");
      else if (r.status === "queued") toast.success("Email is on its way.");
      else toast.error(r.error ?? "The email could not be sent.");
      refresh();
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const pendingPicked = rows
    .filter((r) => picked.has(r.lead_id) && r.state === "pending")
    .map((r) => r.lead_id);
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
  const requeue = useMutation({
    mutationFn: () =>
      http.post<{ moved: number }>("/v1/campaigns/{id}/leads/requeue-verification", {
        path: { id },
      }),
    onSuccess: (r) => {
      toast.success(r.moved ? `${r.moved} moved back to pending.` : "No lead passes the rule yet.");
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
        <FilterMenu
          value={filter}
          onChange={setFilter}
          counts={counts}
          skipTip={skipTip}
          rule={reoon.installed}
        />
        {reoon.installed && byRule && (
          <Button
            variant="secondary"
            className="h-7"
            busy={requeue.isPending}
            onClick={() => requeue.mutate()}
          >
            Move back to pending
          </Button>
        )}
        {reoon.installed && (
          <div className="w-44">
            <Select
              label="Verification"
              hideLabel
              value={vstatus}
              onChange={setVstatus}
              options={[
                { value: "", label: "Any verification" },
                { value: "none", label: say("reoon", "none") },
                ...STATUSES.map((s) => ({ value: s.code, label: say("reoon", s.code) })),
              ]}
            />
          </div>
        )}
        <div className="ml-auto flex gap-2">
          {pendingPicked.length > 0 && (
            <Button variant="secondary" busy={remove.isPending} onClick={() => remove.mutate()}>
              Remove {pendingPicked.length} pending
            </Button>
          )}
          {needReview && (
            <Button
              variant="secondary"
              busy={approveAll.isPending}
              onClick={() => approveAll.mutate()}
            >
              Approve all that need review
            </Button>
          )}
          {camp.data?.content_mode !== "template" && (
            <Button variant="secondary" onClick={() => setGenOpen(true)}>
              Generate emails{picked.size > 0 ? ` for ${picked.size}` : ""}
            </Button>
          )}
          <Button variant="secondary" onClick={() => setMenu("list")}>
            Add from a list
          </Button>
          <MenuRoot>
            <MenuTrigger className="inline-flex h-9 items-center gap-1.5 rounded-(--radius-control) border border-border px-3.5 text-sm font-medium hover:bg-surface-2">
              Add Leads
              <ChevronDown size={14} aria-hidden="true" />
            </MenuTrigger>
            <MenuContent align="end">
              <MenuItem onSelect={() => setMenu("manual")}>Manual entry</MenuItem>
              <MenuItem onSelect={() => setMenu("import")}>CSV upload</MenuItem>
            </MenuContent>
          </MenuRoot>
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
        <EmptyState text="No leads here yet. Add leads from a list, import a CSV file or enter one by hand." />
      )}
      {rows.length > 0 && (
        <Table>
          <Head
            cols={[
              <input
                key="all"
                type="checkbox"
                aria-label="Select all leads on this page"
                checked={rows.length > 0 && rows.every((r) => picked.has(r.lead_id))}
                ref={(el) => {
                  if (el)
                    el.indeterminate = picked.size > 0 && !rows.every((r) => picked.has(r.lead_id));
                }}
                onChange={(e) =>
                  setPicked(e.target.checked ? new Set(rows.map((r) => r.lead_id)) : new Set())
                }
              />,
              ...cols.all.map((c) => <ColumnHeader key={c.id} col={c} />),
              ...(onlyMissing ? ["Fill in"] : []),
              <AddColumn key="add" />,
              <span key="send" className="sr-only">
                Send now
              </span>,
            ]}
          />
          <tbody>
            {rows.map((r) => (
              <tr key={r.lead_id} className="border-t border-border">
                <td className={TD}>
                  <div className="flex items-center gap-2">
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
                    <a
                      aria-label={`Open ${r.email}`}
                      title="Open lead"
                      className="rounded p-1 text-muted hover:bg-surface-2"
                      href={`/w/${ws}/leads?lead=${r.lead_id}`}
                    >
                      <Maximize2 size={13} />
                    </a>
                  </div>
                </td>
                {cols.all.map((c) => (
                  <td key={c.id} className={TD}>
                    {c.id.startsWith("email:") ? (
                      <EmailCell
                        cell={r.emails.find((x) => `email:${x.step_number}` === c.id)}
                        onOpen={() =>
                          setOpenEmail({
                            lead: r.lead_id,
                            email: r.email,
                            step: Number(c.id.slice(6)),
                          })
                        }
                      />
                    ) : c.id === "reoon" ? (
                      <VerifyPill
                        status={r.verification_status}
                        score={r.verification_score}
                        checkedAt={r.verification_checked_at}
                      />
                    ) : c.id === "opened" ? (
                      <OpenPill status={r.open_status} count={r.open_count} />
                    ) : c.id === "state" ? (
                      <Pill>{say("campaignLead", r.state)}</Pill>
                    ) : c.id === "follow" ? (
                      followText(r, camp.data?.content_mode !== "template")
                    ) : c.id === "interest" ? (
                      r.interest ? (
                        <Pill tone="accent">{say("interest", r.interest)}</Pill>
                      ) : (
                        ""
                      )
                    ) : (
                      <EditableCell
                        lead={{ ...r, id: r.lead_id }}
                        col={c}
                        inCampaign
                        locked={
                          !!r.enrollment_id &&
                          ["active", "waiting_send", "paused"].includes(r.enrollment_status ?? "")
                        }
                      />
                    )}
                  </td>
                ))}
                {onlyMissing && (
                  <td className={TD}>
                    <div className="flex flex-col gap-1">
                      {r.missing.map((name) => {
                        const known = cols.all.find(
                          (c) => c.id === name || c.id === `custom:${name}`,
                        );
                        return (
                          <div key={name} className="flex items-center gap-2">
                            <span className="w-28 truncate text-xs text-muted">{name}</span>
                            <EditableCell
                              lead={{ ...r, id: r.lead_id }}
                              col={known ?? { id: name, label: name }}
                              inCampaign
                            />
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
                <td className={TD}>
                  <Button
                    variant="secondary"
                    className="h-7 whitespace-nowrap"
                    aria-label={`Send now to ${r.email}`}
                    busy={sendNow.isPending && sendNow.variables === r.lead_id}
                    disabled={sendNow.isPending}
                    onClick={() => sendNow.mutate(r.lead_id)}
                  >
                    Send now
                  </Button>
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
        onClose={() => {
          setMenu(null);
          setListId("");
          refresh();
        }}
        size="md"
        title="Add leads from a list"
        footer={
          pickedList && pickedList.row_count === 0 && pickedList.member_count > 0 ? (
            <>
              <Button variant="secondary" onClick={() => setMenu(null)}>
                Cancel
              </Button>
              <Button busy={addList.isPending} onClick={() => addList.mutate()}>
                Add leads
              </Button>
            </>
          ) : undefined
        }
      >
        <div className="flex flex-col gap-4">
          <Select
            label="List"
            value={listId}
            onChange={setListId}
            options={[
              { value: "", label: "Choose..." },
              ...(lists.data ?? []).map((l) => ({
                value: l.id,
                label: `${l.name} (${l.row_count || l.member_count})`,
              })),
            ]}
          />
          {pickedList && pickedList.row_count > 0 && (
            <>
              <p className="text-xs text-muted">
                Next you match the list columns to lead columns. You can make new columns. The list
                does not change.
              </p>
              <ListToCampaign key={listId} listId={listId} campaignId={id} onFinished={refresh} />
            </>
          )}
          {pickedList && pickedList.row_count === 0 && pickedList.member_count === 0 && (
            <p className="text-sm text-muted">This list is empty.</p>
          )}
          <p className="text-xs text-muted">
            To add leads by filter, use Leads, select them and choose Add to campaign.
          </p>
        </div>
      </Modal>
      <ManualLeadModal
        open={menu === "manual"}
        onClose={() => {
          setMenu(null);
          refresh();
        }}
        campaignId={id}
      />
      <ImportModal
        open={menu === "import"}
        onClose={() => {
          setMenu(null);
          refresh();
        }}
        campaignId={id}
      />
      {camp.data && (
        <GenerateDialog
          open={genOpen}
          onClose={() => setGenOpen(false)}
          campaign={camp.data}
          selected={[...picked]}
        />
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
    <button
      type="button"
      onClick={onOpen}
      className="flex max-w-56 flex-col items-start gap-0.5 text-left"
    >
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

const FILTER_LABEL: Record<Exclude<Filter, (typeof STATES)[number]>, string> = {
  "": "All leads",
  missing: "Missing a value used in your emails",
  rule: say("skip", "verification_not_allowed"),
};

function filterLabel(f: Filter): string {
  return f === "" || f === "missing" || f === "rule" ? FILTER_LABEL[f] : say("campaignLead", f);
}

function FilterMenu({
  value,
  onChange,
  counts,
  skipTip,
  rule,
}: {
  value: Filter;
  onChange: (f: Filter) => void;
  counts: S["CampaignDetail"]["counts"] | undefined;
  skipTip: string;
  rule: boolean;
}) {
  const count =
    value !== "" && (STATES as readonly string[]).includes(value)
      ? counts?.[value as (typeof STATES)[number]]
      : undefined;
  return (
    <MenuRoot>
      <MenuTrigger
        aria-label={`Filter: ${filterLabel(value)}`}
        className={`group inline-flex h-9 max-w-80 items-center gap-2 rounded-(--radius-control) border px-3 text-sm shadow-(--shadow-card) hover:bg-surface-2 data-[state=open]:bg-surface-2 ${value ? "border-accent/60 bg-accent-soft" : "border-border bg-surface"}`}
      >
        <ListFilter size={15} aria-hidden="true" className={value ? "text-accent" : "text-muted"} />
        <span className="truncate">{filterLabel(value)}</span>
        {count !== undefined && <span className="text-muted tabular-nums">{count}</span>}
        <ChevronDown
          size={14}
          aria-hidden="true"
          className="shrink-0 text-muted transition-transform duration-200 group-data-[state=open]:rotate-180"
        />
      </MenuTrigger>
      <MenuContent align="start" className="min-w-72">
        <MenuRadioGroup value={value} onValueChange={(v) => onChange(v as Filter)}>
          <MenuRadioItem value="">All</MenuRadioItem>
          {STATES.map((s) => (
            <MenuRadioItem key={s} value={s} title={s === "skipped" ? skipTip : undefined}>
              <span className="flex flex-1 items-center justify-between gap-6">
                {say("campaignLead", s)}
                <span className="text-xs text-muted tabular-nums">{counts ? counts[s] : ""}</span>
              </span>
            </MenuRadioItem>
          ))}
          <MenuSeparator />
          <MenuRadioItem value="missing">{FILTER_LABEL.missing}</MenuRadioItem>
          {rule && <MenuRadioItem value="rule">{FILTER_LABEL.rule}</MenuRadioItem>}
        </MenuRadioGroup>
      </MenuContent>
    </MenuRoot>
  );
}
