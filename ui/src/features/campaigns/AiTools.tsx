import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { Button, Check2, ErrorNote, Modal, Notice, Select } from "@/components/ui";
import { fmtInt } from "@/lib/format";
import { stepName } from "./EmailDrawer";

export const runsQuery = (id: string) => ({
  queryKey: ["campaigns", id, "runs"],
  queryFn: () => http.get<S["EmailRun"][]>("/v1/campaigns/{id}/email-runs", { path: { id } }),
  refetchInterval: (query: { state: { data?: S["EmailRun"][] | undefined } }) =>
    query.state.data?.some((r) => r.status === "queued" || r.status === "running") ? 2500 : false,
});

/** The progress of the latest run that is not finished. Shown in the Editor and the Leads tab. */
export function RunProgress({ campaignId }: { campaignId: string }) {
  const qc = useQueryClient();
  const runs = useQuery(runsQuery(campaignId));
  const active = runs.data?.find((r) => r.status === "queued" || r.status === "running");
  const last = runs.data?.[0];
  const cancel = useMutation({
    mutationFn: (run: string) =>
      http.post<S["EmailRun"]>("/v1/campaigns/{id}/email-runs/{run}/cancel", {
        path: { id: campaignId, run },
        body: {},
      }),
    onSuccess: () => {
      void qc.invalidateQueries({ queryKey: ["campaigns", campaignId] });
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  // When a run ends, refresh the Leads tab once.
  const [wasActive, setWasActive] = useState(false);
  useEffect(() => {
    if (active) setWasActive(true);
    else if (wasActive) {
      setWasActive(false);
      void qc.invalidateQueries({ queryKey: ["campaigns", campaignId] });
    }
  }, [active, wasActive, qc, campaignId]);
  if (!active) return null;
  const pct = active.total > 0 ? Math.round(((active.done + active.failed) / active.total) * 100) : 0;
  return (
    <div className="flex flex-col gap-1.5 rounded-(--radius-card) border border-border p-3 text-sm" role="status">
      <div className="flex items-center justify-between gap-3">
        <span>
          Writing emails: {fmtInt(active.done + active.failed)} of {fmtInt(active.total)}
          {active.failed > 0 ? ` (${active.failed} failed)` : ""}. You can leave this page.
        </span>
        <Button variant="secondary" className="h-7" busy={cancel.isPending} onClick={() => cancel.mutate(active.id)}>
          Cancel
        </Button>
      </div>
      <div className="h-1.5 overflow-hidden rounded-full bg-surface-2">
        <div className="h-full bg-accent" style={{ width: `${pct}%` }} />
      </div>
      {last && last.id !== active.id ? null : null}
    </div>
  );
}

type Overwrite = "skip" | "keep_edited" | "all";
type Who = "count" | "selected" | "all";

/** The Generate dialog. `selected` lead ids come from the Leads tab. */
export function GenerateDialog({
  open,
  onClose,
  campaign,
  selected,
}: {
  open: boolean;
  onClose: () => void;
  campaign: S["CampaignDetail"];
  selected?: string[] | undefined;
}) {
  const qc = useQueryClient();
  const steps = campaign.steps;
  const [picked, setPicked] = useState<number[]>([1]);
  const [who, setWho] = useState<Who>(selected && selected.length > 0 ? "selected" : "count");
  const [count, setCount] = useState(50);
  const [overwrite, setOverwrite] = useState<Overwrite>("skip");
  useEffect(() => {
    if (open) setWho(selected && selected.length > 0 ? "selected" : "count");
  }, [open, selected]);

  const body: S["GenerateBody"] = {
    steps: picked,
    leads:
      who === "count"
        ? { count }
        : who === "selected"
          ? { lead_ids: selected ?? [] }
          : {},
    overwrite,
  };
  const est = useQuery({
    queryKey: ["campaigns", campaign.id, "estimate", JSON.stringify(body)],
    enabled: open && picked.length > 0,
    queryFn: () =>
      http.post<S["Estimate"]>("/v1/campaigns/{id}/emails/estimate", {
        path: { id: campaign.id },
        body,
      }),
  });
  const start = useMutation({
    mutationFn: () =>
      http.post<S["EmailRun"]>("/v1/campaigns/{id}/emails/generate", {
        path: { id: campaign.id },
        body,
      }),
    onSuccess: (run) => {
      toast.success(run.total === 0 ? "Nothing to write." : `Writing ${run.total} emails.`);
      void qc.invalidateQueries({ queryKey: ["campaigns", campaign.id] });
      onClose();
    },
  });
  const toggle = (n: number, on: boolean) => setPicked((p) => (on ? [...p, n].sort() : p.filter((x) => x !== n)));
  const e = est.data;
  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Generate emails"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button busy={start.isPending} disabled={picked.length === 0 || (who === "selected" && !selected?.length)} onClick={() => start.mutate()}>
            Generate
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <fieldset className="flex flex-col gap-1.5">
          <legend className="mb-1 text-sm font-medium">What</legend>
          {steps.map((s) => (
            <Check2
              key={s.step_number}
              label={stepName(s.step_number)}
              checked={picked.includes(s.step_number)}
              onChange={(on) => toggle(s.step_number, on)}
            />
          ))}
          {picked.some((n) => n > 1) && !picked.includes(1) && (
            <p className="text-xs text-muted">
              A follow up written before its primary email cannot use {"{{previous_emails}}"}: it is empty.
            </p>
          )}
        </fieldset>
        <fieldset className="flex flex-col gap-1.5">
          <legend className="mb-1 text-sm font-medium">For</legend>
          <label className="flex items-center gap-2 text-sm">
            <input type="radio" name="who" checked={who === "count"} onChange={() => setWho("count")} />
            First
            <div className="w-24">
              <input
                type="number"
                aria-label="How many leads"
                min={1}
                max={10000}
                value={count}
                onChange={(e) => setCount(Math.max(1, Number(e.target.value) || 1))}
                className="h-8 w-full rounded border border-border bg-surface-2 px-2"
              />
            </div>
            leads without this email
          </label>
          <label className="flex items-center gap-2 text-sm">
            <input
              type="radio"
              name="who"
              checked={who === "selected"}
              disabled={!selected?.length}
              onChange={() => setWho("selected")}
            />
            Selected leads {selected?.length ? `(${selected.length})` : "(select some in the Leads tab)"}
          </label>
          <label className="flex items-center gap-2 text-sm">
            <input type="radio" name="who" checked={who === "all"} onChange={() => setWho("all")} />
            All leads in this campaign
          </label>
        </fieldset>
        <Select
          label="Existing emails"
          value={overwrite}
          onChange={(v) => setOverwrite(v as Overwrite)}
          options={[
            { value: "skip", label: "Skip (keep what is written)" },
            { value: "keep_edited", label: "Replace, except edited by hand" },
            { value: "all", label: "Replace all" },
          ]}
        />
        <div className="rounded-(--radius-control) bg-surface-2 p-3 text-sm" aria-live="polite">
          {est.isPending && "Counting..."}
          {est.isError && <ErrorNote error={est.error} />}
          {e && (
            <>
              About {fmtInt(e.emails)} email{e.emails === 1 ? "" : "s"}, about {fmtInt(Math.round(e.tokens / 1000))}k tokens
              {e.cost_known ? `, about $${e.cost_usd.toFixed(2)}` : ", price unknown (add one in the AI settings)"}.
              {e.edited_by_hand > 0 && overwrite === "all" ? ` This replaces ${e.edited_by_hand} edited by hand.` : ""}
            </>
          )}
        </div>
        {start.isError && <Notice tone="danger">{messageOf(start.error)}</Notice>}
      </div>
    </Modal>
  );
}

/** Try the prompts on one lead. Saves nothing. */
export function TestDialog({
  open,
  onClose,
  campaign,
  step,
}: {
  open: boolean;
  onClose: () => void;
  campaign: S["CampaignDetail"];
  step: number;
}) {
  const leads = useQuery({
    queryKey: ["campaigns", campaign.id, "test-leads"],
    enabled: open,
    queryFn: () => http.get<S["CampaignLeadsReply"]>("/v1/campaigns/{id}/leads", { path: { id: campaign.id }, query: { limit: 50 } }),
  });
  const [lead, setLead] = useState("");
  const run = useMutation({
    mutationFn: () =>
      http.post<S["EmailTestReply"]>("/v1/campaigns/{id}/emails/test", {
        path: { id: campaign.id },
        body: { lead_id: lead || (leads.data?.leads[0]?.lead_id ?? ""), step },
      }),
  });
  const r = run.data;
  return (
    <Modal
      open={open}
      onClose={onClose}
      size="lg"
      title={`Test: ${stepName(step)}`}
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Close
          </Button>
          <Button busy={run.isPending} disabled={!leads.data?.leads.length} onClick={() => run.mutate()}>
            Run the test
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        {leads.data && leads.data.leads.length === 0 && <Notice tone="info">Add a lead to this campaign first.</Notice>}
        <Select
          label="Lead"
          value={lead || (leads.data?.leads[0]?.lead_id ?? "")}
          onChange={setLead}
          options={(leads.data?.leads ?? []).map((l) => ({ value: l.lead_id, label: l.email }))}
          help="Nothing is saved. Only the model call is counted."
        />
        {run.isError && <ErrorNote error={run.error} />}
        {r && (
          <div className="flex flex-col gap-3 text-sm">
            <section>
              <h3 className="font-medium">Result</h3>
              {r.subject && <p className="font-medium">{r.subject}</p>}
              <p className="whitespace-pre-wrap">{r.body}</p>
              <p className="mt-1 text-xs text-muted">
                {fmtInt(r.tokens_in)} tokens in, {fmtInt(r.tokens_out)} out.
                {r.warnings.length > 0 ? ` Look at: ${r.warnings.join(" ")}` : ""}
              </p>
            </section>
            {r.user && (
              <details>
                <summary className="cursor-pointer font-medium">The filled prompts</summary>
                <pre className="mt-2 whitespace-pre-wrap rounded bg-surface-2 p-2 text-xs">{r.system}</pre>
                <pre className="mt-2 whitespace-pre-wrap rounded bg-surface-2 p-2 text-xs">{r.user}</pre>
              </details>
            )}
          </div>
        )}
      </div>
    </Modal>
  );
}
