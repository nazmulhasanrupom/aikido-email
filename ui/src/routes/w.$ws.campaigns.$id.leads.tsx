import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { MoreHorizontal } from "lucide-react";
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
import { ImportModal } from "@/features/leads/ImportModal";
import { fmtDateTime, fullName } from "@/lib/format";
import { say } from "@/lib/words";

export const Route = createFileRoute("/w/$ws/campaigns/$id/leads")({ component: CampaignLeads });

const STATES = ["pending", "enrolled", "skipped"] as const;

function followText(r: S["CampaignLeadRow"]): string {
  if (r.state === "pending") return "Waiting to start";
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
  const [state, setState] = useState<string>("");
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [menu, setMenu] = useState<null | "list" | "import">(null);
  const [listId, setListId] = useState("");
  const leads = useInfiniteQuery({
    queryKey: ["campaigns", id, "leads", state],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      http.get<S["CampaignLeadsReply"]>("/v1/campaigns/{id}/leads", {
        path: { id },
        query: { state, limit: 50, cursor: pageParam },
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
  const remove = useMutation({
    mutationFn: () =>
      http.del("/v1/campaigns/{id}/leads", { path: { id }, body: { lead_ids: [...picked] } }),
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
        <div className="ml-auto flex gap-2">
          {picked.size > 0 && (
            <Button variant="secondary" busy={remove.isPending} onClick={() => remove.mutate()}>
              Remove {picked.size} pending
            </Button>
          )}
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
      {leads.isPending && <Skeleton />}
      {leads.isError && <ErrorNote error={leads.error} />}
      {leads.data && rows.length === 0 && (
        <EmptyState text="No leads here yet. Add leads from a list or import a CSV file." />
      )}
      {rows.length > 0 && (
        <Table>
          <Head cols={["", "Email", "Name", "Company", "State", "Follow-up", "Interest", ""]} />
          <tbody>
            {rows.map((r) => (
              <tr key={r.lead_id} className="border-t border-border">
                <td className={TD}>
                  {r.state === "pending" && (
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
                  )}
                </td>
                <td className={TD}>
                  <a
                    className="font-medium hover:underline"
                    href={`/w/${ws}/leads?lead=${r.lead_id}`}
                  >
                    {r.email}
                  </a>
                </td>
                <td className={TD}>{fullName(r)}</td>
                <td className={TD}>{r.company_name}</td>
                <td className={TD}>
                  <Pill>{say("campaignLead", r.state)}</Pill>
                </td>
                <td className={TD}>{followText(r)}</td>
                <td className={TD}>
                  {r.interest ? <Pill tone="accent">{say("interest", r.interest)}</Pill> : ""}
                </td>
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
    </div>
  );
}
