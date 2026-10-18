import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { Settings } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { http } from "@/api/http";
import { q } from "@/api/queries";
import {
  Button,
  ConfirmDialog,
  EmptyState,
  ErrorNote,
  Field,
  Head,
  Notice,
  Page,
  PageHeader,
  Select,
  Skeleton,
  Table,
  TD,
  useDebounced,
} from "@/components/ui";
import { LeadDrawer } from "@/features/leads/LeadDrawer";
import { useUser } from "@/features/settings/useUser";
import { fmtAgo, fmtInt, fullName } from "@/lib/format";
import { PROBLEM_WORDS, say } from "@/lib/words";
import {
  type Balance,
  type Run,
  reoonSettingsQuery,
  STATUSES,
  type VerifyRow,
  useReoon,
} from "./api";
import { patchSettings, problemText, RuleDialog, SettingsDialog } from "./dialogs";
import { VerifyPill } from "./parts";

export type ReoonSearch = {
  q?: string;
  list_id?: string;
  tag?: string;
  campaign_id?: string;
  status?: string;
  checked?: string;
  lead?: string;
};

const DAY = 86_400_000;

function ruleText(allowed: string[], notVerified: boolean): string {
  const parts = STATUSES.filter((s) => allowed.includes(s.code)).map((s) => say("reoon", s.code));
  if (notVerified) parts.push("Not verified");
  return parts.length ? parts.join(", ") : "Nothing";
}

export function ReoonPage({ ws, search }: { ws: string; search: ReoonSearch }) {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const { isAdmin } = useUser();
  const reoon = useReoon();
  const settings = useQuery({ ...reoonSettingsQuery, enabled: reoon.installed });
  const lists = useQuery(q.lists);
  const campaigns = useQuery(q.campaigns);
  const [text, setText] = useState(search.q ?? "");
  const debounced = useDebounced(text, 250);
  const [picked, setPicked] = useState<Set<string>>(new Set());
  const [allMatching, setAllMatching] = useState(false);
  const [gear, setGear] = useState(false);
  const [rule, setRule] = useState(false);
  const [confirm, setConfirm] = useState<null | "verify" | "again" | "block">(null);

  const balance = useQuery({
    queryKey: ["reoon", "balance"],
    enabled: reoon.installed && reoon.status === "installed",
    queryFn: () => http.get<Balance>("/v1/verify/reoon/balance"),
    refetchInterval: 60_000,
    retry: false,
  });
  const runs = useQuery({
    queryKey: ["reoon", "runs", "bar"],
    enabled: reoon.installed,
    queryFn: async () => (await http.get<{ runs: Run[] }>("/v1/verify/reoon/runs")).runs,
    refetchInterval: (query) =>
      query.state.data?.some((r) => r.status === "queued" || r.status === "running") ? 2000 : 8000,
  });

  // when a run ends, load the new results at once
  const wasActive = useRef(false);
  const isActive = !!runs.data?.some((r) => r.status === "queued" || r.status === "running");
  useEffect(() => {
    if (wasActive.current && !isActive) void qc.invalidateQueries({ queryKey: ["reoon", "leads"] });
    wasActive.current = isActive;
  }, [isActive, qc]);

  const filter: Record<string, unknown> = {
    q: debounced || undefined,
    list_id: search.list_id,
    tag: search.tag,
    campaign_id: search.campaign_id,
    verification_status: search.status,
    checked_before: search.checked
      ? new Date(Date.now() - Number(search.checked) * DAY).toISOString()
      : undefined,
  };
  const leads = useInfiniteQuery({
    queryKey: ["reoon", "leads", filter],
    enabled: reoon.installed,
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      http.get<{ leads: VerifyRow[]; next_cursor: string | null }>("/v1/verify/reoon/leads", {
        query: { ...filter, limit: 50, cursor: pageParam },
      }),
    getNextPageParam: (last) => last.next_cursor ?? undefined,
    refetchInterval: isActive ? 4000 : false,
  });
  const rows = leads.data?.pages.flatMap((p) => p.leads) ?? [];
  const setFilter = (k: keyof ReoonSearch, v: string) =>
    navigate({
      to: "/w/$ws/apps/reoon",
      params: { ws },
      search: { ...search, [k]: v || undefined },
      replace: true,
    });
  const allOnPage = rows.length > 0 && rows.every((r) => picked.has(r.id));

  const cfg = settings.data?.settings;
  const reuseMs = (cfg?.reuse_days ?? 30) * DAY;
  const chosen = rows.filter((r) => picked.has(r.id));
  const recent = chosen.filter((r) => r.result && Date.now() - new Date(r.result.checked_at).getTime() < reuseMs).length;
  const credits = (balance.data?.daily_credits ?? 0) + (balance.data?.instant_credits ?? 0);
  const estimate = (again: boolean) => (again ? chosen.length : chosen.length - recent);

  const selection = () =>
    allMatching ? { filter: { q: filter.q, list_id: filter.list_id, tag: filter.tag, campaign_id: filter.campaign_id, verification_status: filter.verification_status }, checked_before: filter.checked_before } : { lead_ids: [...picked] };
  const start = useMutation({
    mutationFn: (force: boolean) =>
      http.post<{ run: Run; estimated_credits: number }>("/v1/verify/reoon/runs", {
        body: { ...selection(), force, idempotency_key: `${Date.now()}-${Math.random().toString(36).slice(2)}` },
      }),
    onSuccess: (r) => {
      toast.success(`Checking ${fmtInt(r.run.total)} emails (about ${fmtInt(r.estimated_credits)} credits).`);
      setPicked(new Set());
      setAllMatching(false);
      void qc.invalidateQueries({ queryKey: ["reoon"] });
    },
    onError: (e) => toast.error(problemText(e)),
  });
  const runAct = useMutation({
    mutationFn: ({ id, what }: { id: string; what: "cancel" | "resume" }) =>
      http.post(`/v1/verify/reoon/runs/{id}/${what}`, { path: { id } }),
    onSuccess: () => void qc.invalidateQueries({ queryKey: ["reoon"] }),
    onError: (e) => toast.error(problemText(e)),
  });
  const saveRule = (r: { allowed: string[]; allow_not_verified: boolean }) =>
    patchSettings({ send_rule: r }).then(() => qc.invalidateQueries({ queryKey: reoonSettingsQuery.queryKey }));

  if (reoon.loading) return <Page wide><Skeleton /></Page>;
  if (!reoon.installed)
    return (
      <Page wide>
        <PageHeader title="Reoon Email Verifier" />
        <EmptyState
          text="Reoon is not installed."
          action={
            <Link to="/w/$ws/settings/integrations" params={{ ws }} className="text-accent underline">
              Install it in Settings, Integrations
            </Link>
          }
        />
      </Page>
    );

  const active = runs.data?.find((r) => r.status === "queued" || r.status === "running");
  const stopped = runs.data?.[0]?.status === "failed" ? runs.data[0] : undefined;
  const bar = active ?? stopped;
  const pct = bar && bar.total ? Math.round(((bar.done_count + bar.skipped_count + bar.failed_count) / bar.total) * 100) : 0;

  return (
    <Page wide>
      <PageHeader
        title="Reoon Email Verifier"
        sub={
          balance.data
            ? `Credits: ${fmtInt(balance.data.daily_credits)} today, ${fmtInt(balance.data.instant_credits)} instant`
            : balance.isError
              ? "Credits: not available"
              : "Credits: loading"
        }
        actions={
          <Button variant="secondary" aria-label="Reoon settings" onClick={() => setGear(true)}>
            <Settings size={16} /> Settings
          </Button>
        }
      />

      {reoon.status === "auth_failed" && (
        <div className="mb-3">
          <Notice tone="danger">{PROBLEM_WORDS.auth_failed}</Notice>
        </div>
      )}

      {cfg && (
        <div className="mb-3 flex flex-wrap items-center gap-3 rounded-(--radius-card) border border-border px-4 py-3">
          <span>
            <span className="font-medium">Send rule:</span> {ruleText(cfg.send_rule.allowed, cfg.send_rule.allow_not_verified)}
          </span>
          <Button variant="secondary" className="h-7" onClick={() => setRule(true)}>
            Edit rule
          </Button>
          <span className="text-xs text-muted">The rule applies to leads that start a campaign after now.</span>
          <label className="ml-auto flex items-center gap-2 text-sm">
            <input
              type="checkbox"
              role="switch"
              checked={cfg.auto_verify}
              onChange={(e) =>
                patchSettings({ auto_verify: e.target.checked }).then(() => {
                  toast.success(e.target.checked ? "New leads will be verified." : "Verify on import is off.");
                  void qc.invalidateQueries({ queryKey: reoonSettingsQuery.queryKey });
                })
              }
              className="size-4 accent-(--accent)"
            />
            Verify new leads automatically
          </label>
        </div>
      )}

      {bar && (
        <div className="mb-3 flex flex-wrap items-center gap-3 rounded-(--radius-card) bg-accent-soft px-4 py-3" role="status">
          {bar.status === "failed" ? (
            <>
              <span className="font-medium">
                Stopped: {PROBLEM_WORDS[bar.error ?? ""] ?? bar.error ?? "the run stopped"}
              </span>
              <Button className="h-7" onClick={() => runAct.mutate({ id: bar.id, what: "resume" })}>
                Resume
              </Button>
            </>
          ) : (
            <>
              <span className="font-medium">
                Running: {fmtInt(bar.done_count + bar.skipped_count + bar.failed_count)} of {fmtInt(bar.total)}
                {bar.auto ? " (new leads)" : ""}
              </span>
              <progress value={pct} max={100} className="h-2 w-48" aria-label="Progress" />
              <Button variant="secondary" className="h-7" onClick={() => runAct.mutate({ id: bar.id, what: "cancel" })}>
                Cancel
              </Button>
            </>
          )}
          {bar.warning && <span className="text-xs text-muted">{bar.warning}</span>}
        </div>
      )}

      <div className="mb-3 flex flex-wrap items-end gap-2">
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
            options={[{ value: "", label: "All lists" }, ...(lists.data ?? []).map((l) => ({ value: l.id, label: l.name }))]}
          />
        </div>
        <Field label="Tag" className="w-32" value={search.tag ?? ""} onChange={(e) => setFilter("tag", e.target.value)} />
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
        <div className="w-44">
          <Select
            label="Status"
            value={search.status ?? ""}
            onChange={(v) => setFilter("status", v)}
            options={[
              { value: "", label: "Any status" },
              { value: "none", label: say("reoon", "none") },
              ...STATUSES.map((s) => ({ value: s.code, label: say("reoon", s.code) })),
            ]}
          />
        </div>
        <div className="w-48">
          <Select
            label="Checked"
            value={search.checked ?? ""}
            onChange={(v) => setFilter("checked", v)}
            options={[
              { value: "", label: "Any time" },
              { value: "7", label: "Not in 7 days" },
              { value: "30", label: "Not in 30 days" },
              { value: "90", label: "Not in 90 days" },
            ]}
          />
        </div>
      </div>

      {(picked.size > 0 || allMatching) && (
        <div className="mb-3 flex flex-wrap items-center gap-2 rounded-(--radius-control) bg-accent-soft px-3 py-2" role="toolbar" aria-label="Actions for the selected leads">
          <span className="font-medium">{allMatching ? "All leads that match the filter" : `${picked.size} selected`}</span>
          {allOnPage && !allMatching && leads.hasNextPage && (
            <Button variant="ghost" className="h-7" onClick={() => setAllMatching(true)}>
              Select all that match the filter
            </Button>
          )}
          <Button className="h-7" onClick={() => setConfirm("verify")}>
            Verify
          </Button>
          <Button variant="secondary" className="h-7" onClick={() => setConfirm("again")}>
            Verify again
          </Button>
          <Button variant="danger" className="h-7" disabled={allMatching} onClick={() => setConfirm("block")}>
            Block these
          </Button>
          <Button variant="ghost" className="h-7" onClick={() => { setPicked(new Set()); setAllMatching(false); }}>
            Clear
          </Button>
        </div>
      )}

      {leads.isPending && <Skeleton />}
      {leads.isError && <ErrorNote error={leads.error} onRetry={() => leads.refetch()} />}
      {leads.data && rows.length === 0 && (
        <EmptyState
          text={search.status || search.q || search.list_id || search.tag || search.campaign_id || search.checked ? "No leads match." : "Import leads first."}
          action={
            <Link to="/w/$ws/leads" params={{ ws }} className="text-accent underline">
              Go to Leads
            </Link>
          }
        />
      )}
      {rows.length > 0 && (
        <Table>
          <Head
            cols={[
              <input
                key="all"
                type="checkbox"
                aria-label="Select all on this page"
                checked={allOnPage}
                onChange={(e) => setPicked(e.target.checked ? new Set(rows.map((r) => r.id)) : new Set())}
              />,
              "Email",
              "Name",
              "Company",
              "Verification",
              "Score",
              "Checked",
            ]}
          />
          <tbody>
            {rows.map((r) => (
              <tr key={r.id} className="cursor-pointer border-t border-border hover:bg-surface-2/50" onClick={() => setFilter("lead", r.id)}>
                <td className={TD} onClick={(e) => e.stopPropagation()}>
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
                <td className={TD}>{r.email}</td>
                <td className={TD}>{fullName(r)}</td>
                <td className={TD}>{r.company_name}</td>
                <td className={TD}>
                  <VerifyPill status={r.result?.status} score={r.result?.score} checkedAt={r.result?.checked_at} />
                </td>
                <td className={TD}>{r.result?.score ?? "-"}</td>
                <td className={TD}>{r.result ? fmtAgo(r.result.checked_at) : "-"}</td>
              </tr>
            ))}
          </tbody>
        </Table>
      )}
      {leads.hasNextPage && (
        <div className="mt-3">
          <Button variant="secondary" busy={leads.isFetchingNextPage} onClick={() => leads.fetchNextPage()}>
            Load more
          </Button>
        </div>
      )}

      <SettingsDialog open={gear} onClose={() => setGear(false)} isAdmin={isAdmin} />
      {cfg && <RuleDialog open={rule} onClose={() => setRule(false)} rule={cfg.send_rule} onSave={saveRule} />}
      <ConfirmDialog
        open={confirm === "verify" || confirm === "again"}
        onClose={() => setConfirm(null)}
        title={confirm === "again" ? "Verify again?" : "Verify these emails?"}
        danger={false}
        confirmLabel="Verify"
        text={
          allMatching
            ? "Every lead that matches the filter is checked. Emails with a recent result are skipped and cost nothing."
            : `About ${fmtInt(Math.max(0, estimate(confirm === "again")))} credits. You have ${fmtInt(credits)}.`
        }
        onConfirm={() => start.mutateAsync(confirm === "again")}
      />
      <ConfirmDialog
        open={confirm === "block"}
        onClose={() => setConfirm(null)}
        title="Block these emails?"
        confirmLabel="Block"
        text={`${chosen.length} addresses are added to the blocklist. They are never emailed again.`}
        onConfirm={async () => {
          for (const r of chosen) {
            await http.post("/v1/sending/suppressions", { body: { kind: "email", value: r.email, reason: "manual" } });
          }
          toast.success(`${chosen.length} blocked.`);
          setPicked(new Set());
          void qc.invalidateQueries({ queryKey: ["leads"] });
        }}
      />
      <LeadDrawer id={search.lead ?? null} ws={ws} onClose={() => setFilter("lead", "")} />
    </Page>
  );
}

