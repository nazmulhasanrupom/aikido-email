import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useNavigate } from "@tanstack/react-router";
import { Pencil } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { ApiProblem, messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import {
  Button,
  ConfirmDialog,
  EmptyState,
  ErrorNote,
  Field,
  Head,
  Modal,
  Pill,
  Select,
  Skeleton,
  Table,
  TD,
  useDebounced,
} from "@/components/ui";
import { STATUSES, useReoon } from "@/features/reoon/api";
import { VerifyPill } from "@/features/reoon/parts";
import { fmtDate } from "@/lib/format";
import { CONTACT_STATES, INTERESTS, say } from "@/lib/words";
import { ImportModal } from "./ImportModal";
import { AddColumn, ColumnHeader, ColumnsMenu, EditableCell, type GridCol, useColumns } from "./grid";

const BASE_COLS: GridCol[] = [
  { id: "email", label: "Email" },
  { id: "first_name", label: "First name" },
  { id: "last_name", label: "Last name" },
  { id: "company_name", label: "Company" },
  { id: "job_title", label: "Job title" },
  { id: "contact_state", label: "State" },
  { id: "interest", label: "Interest" },
  { id: "verification", label: "Email check" },
  { id: "tags", label: "Tags" },
  { id: "created_at", label: "Created" },
];
/** Columns that are not edited in the cell. */
const STATIC: Record<string, (r: S["Lead"]) => React.ReactNode> = {
  reoon: (r) => <VerifyPill status={r.verification_status} score={r.verification_score} checkedAt={r.verification_checked_at} />,
  contact_state: (r) => say("contact", r.contact_state),
  interest: (r) => (r.interest ? <Pill tone="accent">{say("interest", r.interest)}</Pill> : ""),
  verification: (r) => say("verification", r.verification),
  tags: (r) => r.tags.join(", "),
  created_at: (r) => fmtDate(r.created_at),
};

export type LeadSearch = {
  q?: string;
  list_id?: string;
  tag?: string;
  campaign_id?: string;
  contact_state?: string;
  interest?: string;
  verification?: string;
  suppressed?: string;
  verification_status?: string;
};
const FILTERS = [
  "q",
  "list_id",
  "tag",
  "campaign_id",
  "contact_state",
  "interest",
  "verification",
  "suppressed",
  "verification_status",
] as const;

function toQuery(s: LeadSearch): Record<string, unknown> {
  const out: Record<string, unknown> = {};
  for (const k of FILTERS) if (s[k]) out[k] = k === "suppressed" ? s[k] === "true" : s[k];
  return out;
}

export function AllLeads({ search, ws }: { search: LeadSearch; ws: string }) {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const reoon = useReoon();
  const cols = useColumns("leads", reoon.installed ? [...BASE_COLS, { id: "reoon", label: "Verification" }] : BASE_COLS);
  const lists = useQuery(q.lists);
  const campaigns = useQuery(q.campaigns);
  const [text, setText] = useState(search.q ?? "");
  const debounced = useDebounced(text, 250);
  const filter = toQuery({ ...search, q: debounced || undefined });
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [allMatching, setAllMatching] = useState(false);
  const [adding, setAdding] = useState(false);
  const [importing, setImporting] = useState(false);
  const [erase, setErase] = useState(false);
  const [dialog, setDialog] = useState<null | "list" | "unlist" | "tags" | "untag" | "campaign">(
    null,
  );

  const leads = useInfiniteQuery({
    queryKey: ["leads", "list", filter],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      http.get<S["ListLeadsReply"]>("/v1/leads", {
        query: { ...filter, limit: 50, cursor: pageParam },
      }),
    getNextPageParam: (last) => last.next_cursor ?? undefined,
    retry: (n, e) => !(e instanceof ApiProblem) && n < 2,
  });
  const rows = leads.data?.pages.flatMap((p) => p.leads) ?? [];
  const setFilter = (k: keyof LeadSearch, v: string) =>
    navigate({
      to: "/w/$ws/leads",
      params: { ws },
      search: { ...search, [k]: v || undefined },
      replace: true,
    });
  const open = (id: string) =>
    navigate({ to: "/w/$ws/leads", params: { ws }, search: { ...search, lead: id } });
  const allOnPage = rows.length > 0 && rows.every((r) => picked.has(r.id));

  function selection(): Pick<S["ActionBody"], "lead_ids" | "filter"> {
    return allMatching ? { filter: filter as S["LeadFilter"] } : { lead_ids: [...picked] };
  }
  const count = allMatching ? "all matching" : picked.size;
  const act = useMutation({
    mutationFn: (body: Omit<S["ActionBody"], "lead_ids" | "filter">) =>
      http.post<S["ActionReply"]>("/v1/leads/actions", { body: { ...body, ...selection() } }),
    onSuccess: (r) => {
      toast.success(`${r.changed} lead${r.changed === 1 ? "" : "s"} changed.`);
      setPicked(new Set());
      setAllMatching(false);
      void qc.invalidateQueries({ queryKey: ["leads"] });
    },
    onError: (e) => toast.error(messageOf(e)),
  });

  return (
    <div className="flex flex-col gap-4">
      <div className="flex flex-wrap items-end gap-2">
        <div className="w-64">
          <Field
            label="Search"
            placeholder="Name, email or company"
            value={text}
            onChange={(e) => {
              setText(e.target.value);
              void setFilter("q", e.target.value);
            }}
          />
        </div>
        <div className="w-40">
          <Select
            label="List"
            value={search.list_id ?? ""}
            onChange={(v) => setFilter("list_id", v)}
            options={[
              { value: "", label: "All lists" },
              ...(lists.data ?? []).map((l) => ({ value: l.id, label: l.name })),
            ]}
          />
        </div>
        <div className="w-44">
          <Select
            label="Campaign"
            value={search.campaign_id ?? ""}
            onChange={(v) => setFilter("campaign_id", v)}
            options={[
              { value: "", label: "All campaigns" },
              ...(campaigns.data?.campaigns ?? []).map((c) => ({ value: c.id, label: c.name })),
            ]}
          />
        </div>
        <div className="w-40">
          <Select
            label="State"
            value={search.contact_state ?? ""}
            onChange={(v) => setFilter("contact_state", v)}
            options={[
              { value: "", label: "Any state" },
              ...CONTACT_STATES.map((s) => ({ value: s, label: say("contact", s) })),
            ]}
          />
        </div>
        <div className="w-40">
          <Select
            label="Interest"
            value={search.interest ?? ""}
            onChange={(v) => setFilter("interest", v)}
            options={[
              { value: "", label: "Any interest" },
              ...INTERESTS.map((s) => ({ value: s, label: say("interest", s) })),
            ]}
          />
        </div>
        <div className="w-44">
          <Select
            label="Email check"
            value={search.verification ?? ""}
            onChange={(v) => setFilter("verification", v)}
            options={[
              { value: "", label: "Any" },
              ...["mx_ok", "syntax_ok", "no_mx", "invalid", "unknown"].map((s) => ({
                value: s,
                label: say("verification", s),
              })),
            ]}
          />
        </div>
        {reoon.installed && (
          <div className="w-44">
            <Select
              label="Verification"
              value={search.verification_status ?? ""}
              onChange={(v) => setFilter("verification_status", v)}
              options={[
                { value: "", label: "Any" },
                { value: "none", label: say("reoon", "none") },
                ...STATUSES.map((s) => ({ value: s.code, label: say("reoon", s.code) })),
              ]}
            />
          </div>
        )}
        <div className="w-36">
          <Select
            label="Blocked"
            value={search.suppressed ?? ""}
            onChange={(v) => setFilter("suppressed", v)}
            options={[
              { value: "", label: "Any" },
              { value: "true", label: "Blocked" },
              { value: "false", label: "Not blocked" },
            ]}
          />
        </div>
        <Field
          label="Tag"
          className="w-32"
          value={search.tag ?? ""}
          onChange={(e) => setFilter("tag", e.target.value)}
        />
        <div className="ml-auto flex gap-2">
          <Button variant="secondary" onClick={() => setImporting(true)}>
            Import CSV
          </Button>
          <a
            className="inline-flex h-9 items-center rounded-(--radius-control) border border-border px-3.5 text-sm font-medium hover:bg-surface-2"
            href={`/v1/leads/export.csv?${new URLSearchParams(Object.entries(filter).map(([k, v]) => [k, String(v)])).toString()}`}
            download
          >
            Export
          </a>
          <ColumnsMenu cols={cols} />
          <Button onClick={() => setAdding(true)}>Add lead</Button>
        </div>
      </div>

      {(picked.size > 0 || allMatching) && (
        <div
          className="flex flex-wrap items-center gap-2 rounded-(--radius-control) bg-accent-soft px-3 py-2"
          role="toolbar"
          aria-label="Actions for the selected leads"
        >
          <span className="font-medium">
            {allMatching ? "All leads that match the filter" : `${picked.size} selected`}
          </span>
          {allOnPage && !allMatching && leads.hasNextPage && (
            <Button variant="ghost" className="h-7" onClick={() => setAllMatching(true)}>
              Select all that match the filter
            </Button>
          )}
          <Button variant="secondary" className="h-7" onClick={() => setDialog("list")}>
            Add to list
          </Button>
          <Button variant="secondary" className="h-7" onClick={() => setDialog("unlist")}>
            Remove from list
          </Button>
          <Button variant="secondary" className="h-7" onClick={() => setDialog("tags")}>
            Add tags
          </Button>
          <Button variant="secondary" className="h-7" onClick={() => setDialog("untag")}>
            Remove tags
          </Button>
          <Button
            variant="secondary"
            className="h-7"
            onClick={() => act.mutate({ action: "verify" })}
          >
            Check emails again
          </Button>
          <Button variant="secondary" className="h-7" onClick={() => setDialog("campaign")}>
            Add to campaign
          </Button>
          <Button variant="danger" className="h-7" onClick={() => setErase(true)}>
            Delete
          </Button>
          <Button
            variant="ghost"
            className="h-7"
            onClick={() => {
              setPicked(new Set());
              setAllMatching(false);
            }}
          >
            Clear
          </Button>
        </div>
      )}

      {leads.isPending && <Skeleton />}
      {leads.isError && <ErrorNote error={leads.error} onRetry={() => leads.refetch()} />}
      {leads.data && rows.length === 0 && (
        <EmptyState
          text="No leads match. Add one or import a CSV file."
          action={<Button onClick={() => setImporting(true)}>Import CSV</Button>}
        />
      )}
      {rows.length > 0 && (
        <Table>
          <Head
            cols={[
              "",
              ...cols.visible.map((c) => <ColumnHeader key={c.id} col={c} />),
              <span key="add" className="inline-flex items-center gap-1">
                <AddColumn />
              </span>,
            ]}
          />
          <tbody>
            {rows.map((r) => (
              <tr key={r.id} className="border-t border-border hover:bg-surface-2/50">
                <td className={TD}>
                  <input
                    type="checkbox"
                    aria-label={`Select ${r.email}`}
                    checked={picked.has(r.id) || allMatching}
                    onChange={(e) => {
                      const n = new Set(picked);
                      e.target.checked ? n.add(r.id) : n.delete(r.id);
                      setPicked(n);
                      setAllMatching(false);
                    }}
                  />
                </td>
                {cols.visible.map((c) => (
                  <td key={c.id} className={TD}>
                    {STATIC[c.id] ? (
                      STATIC[c.id]?.(r)
                    ) : (
                      <div className="flex items-center gap-1">
                        <EditableCell lead={r} col={c} />
                        {c.id === "email" && (
                          <button
                            type="button"
                            aria-label={`Open ${r.email}`}
                            title="Open lead"
                            className="rounded p-1 text-muted hover:bg-surface-2"
                            onClick={() => open(r.id)}
                          >
                            <Pencil size={13} />
                          </button>
                        )}
                      </div>
                    )}
                  </td>
                ))}
                <td className={TD} />
              </tr>
            ))}
          </tbody>
        </Table>
      )}
      {leads.hasNextPage && (
        <div>
          <Button
            variant="secondary"
            busy={leads.isFetchingNextPage}
            onClick={() => leads.fetchNextPage()}
          >
            Load more
          </Button>
        </div>
      )}

      <AddLead open={adding} onClose={() => setAdding(false)} />
      <ImportModal open={importing} onClose={() => setImporting(false)} />
      <ActionDialog
        kind={dialog}
        lists={lists.data ?? []}
        campaigns={campaigns.data?.campaigns ?? []}
        onClose={() => setDialog(null)}
        count={count}
        onList={(id, remove) =>
          act.mutateAsync({ action: remove ? "remove_from_list" : "add_to_list", list_id: id })
        }
        onTags={(tags, remove) =>
          act.mutateAsync({ action: remove ? "remove_tags" : "add_tags", tags })
        }
        onCampaign={async (id) => {
          const r = await http.post<S["AddLeadsReply"]>("/v1/campaigns/{id}/leads", {
            path: { id },
            body: selection(),
          });
          toast.success(`${r.added} added to the campaign.`);
          setPicked(new Set());
          setAllMatching(false);
        }}
      />
      <ConfirmDialog
        open={erase}
        onClose={() => setErase(false)}
        title="Delete leads?"
        typed="DELETE"
        confirmLabel="Delete leads"
        text="These leads are deleted and their emails are blocked forever. Their sequences stop."
        onConfirm={() => act.mutateAsync({ action: "delete" })}
      />
      <span className="sr-only" aria-live="polite">
        {rows.length} leads shown
      </span>
    </div>
  );
}

function ActionDialog({
  kind,
  lists,
  campaigns,
  onClose,
  count,
  onList,
  onTags,
  onCampaign,
}: {
  kind: null | "list" | "unlist" | "tags" | "untag" | "campaign";
  lists: S["LeadList"][];
  campaigns: S["CampaignSummary"][];
  onClose: () => void;
  count: number | string;
  onList: (id: string, remove: boolean) => Promise<unknown>;
  onTags: (tags: string[], remove: boolean) => Promise<unknown>;
  onCampaign: (id: string) => Promise<unknown>;
}) {
  const [value, setValue] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const isList = kind === "list" || kind === "unlist";
  const isTag = kind === "tags" || kind === "untag";
  const titles = {
    list: "Add to a list",
    unlist: "Remove from a list",
    tags: "Add tags",
    untag: "Remove tags",
    campaign: "Add to a campaign",
  };
  async function run() {
    setBusy(true);
    setError(null);
    try {
      if (isList) await onList(value, kind === "unlist");
      else if (isTag)
        await onTags(
          value
            .split(",")
            .map((t) => t.trim())
            .filter(Boolean),
          kind === "untag",
        );
      else await onCampaign(value);
      setValue("");
      onClose();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }
  return (
    <Modal
      open={!!kind}
      onClose={onClose}
      size="sm"
      title={kind ? titles[kind] : ""}
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button busy={busy} disabled={!value} onClick={run}>
            Apply to {count}
          </Button>
        </>
      }
    >
      {isTag ? (
        <Field
          label="Tags (separated by commas)"
          value={value}
          onChange={(e) => setValue(e.target.value)}
        />
      ) : (
        <Select
          label={kind === "campaign" ? "Campaign" : "List"}
          value={value}
          onChange={setValue}
          options={[
            { value: "", label: "Choose..." },
            ...(kind === "campaign"
              ? campaigns
                  .filter((c) => c.status !== "archived" && c.status !== "completed")
                  .map((c) => ({ value: c.id, label: c.name }))
              : lists.map((l) => ({ value: l.id, label: l.name }))),
          ]}
        />
      )}
      {error != null && (
        <div className="mt-3">
          <ErrorNote error={error} />
        </div>
      )}
    </Modal>
  );
}

function AddLead({ open, onClose }: { open: boolean; onClose: () => void }) {
  const qc = useQueryClient();
  const [v, setV] = useState({
    email: "",
    first_name: "",
    last_name: "",
    company_name: "",
    job_title: "",
  });
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  async function save() {
    setBusy(true);
    setError(null);
    try {
      await http.post("/v1/leads", {
        body: Object.fromEntries(Object.entries(v).filter(([, x]) => x)),
      });
      await qc.invalidateQueries({ queryKey: ["leads"] });
      setV({ email: "", first_name: "", last_name: "", company_name: "", job_title: "" });
      onClose();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }
  const set = (k: keyof typeof v) => (e: React.ChangeEvent<HTMLInputElement>) =>
    setV({ ...v, [k]: e.target.value });
  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Add a lead"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button busy={busy} disabled={!v.email} onClick={save}>
            Add lead
          </Button>
        </>
      }
    >
      <div className="grid gap-3 sm:grid-cols-2">
        <div className="sm:col-span-2">
          <Field label="Email" type="email" value={v.email} onChange={set("email")} />
        </div>
        <Field label="First name" value={v.first_name} onChange={set("first_name")} />
        <Field label="Last name" value={v.last_name} onChange={set("last_name")} />
        <Field label="Company" value={v.company_name} onChange={set("company_name")} />
        <Field label="Job title" value={v.job_title} onChange={set("job_title")} />
      </div>
      {error != null && (
        <div className="mt-3">
          <ErrorNote error={error} />
        </div>
      )}
    </Modal>
  );
}
