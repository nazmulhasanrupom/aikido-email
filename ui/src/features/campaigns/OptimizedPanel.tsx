import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { CheckCircle2, ChevronDown, Sparkles, XCircle } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { Button, ErrorNote, Modal, Notice, Pill, Select, Skeleton, Tabs, TextArea } from "@/components/ui";
import { GenerateDialog, RunProgress } from "./AiTools";
import { stepName } from "./EmailDrawer";

type Block = "core" | "primary" | "followup";
type Tab = "primary" | "followup";

export const promptsQuery = (id: string) => ({
  queryKey: ["campaigns", id, "prompts"],
  queryFn: () => http.get<S["CampaignPrompts"]>("/v1/campaigns/{id}/prompts", { path: { id } }),
});

/** Optimized | Custom. Optimized: the server builds the user prompt and checks every email. */
export function PromptKindSwitch({ campaign, custom }: { campaign: S["CampaignDetail"]; custom: React.ReactNode }) {
  const { ws } = useParams({ strict: false }) as { ws: string };
  const qc = useQueryClient();
  const prompts = useQuery(promptsQuery(campaign.id));
  const [ask, setAsk] = useState<null | "optimized" | "custom">(null);
  const change = useMutation({
    mutationFn: (kind: "optimized" | "custom") =>
      http.put<S["CampaignPrompts"]>("/v1/campaigns/{id}/prompts", { path: { id: campaign.id }, body: { kind } }),
    onSuccess: async () => {
      setAsk(null);
      await qc.invalidateQueries({ queryKey: ["campaigns", campaign.id] });
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  if (prompts.isPending) return <Skeleton rows={6} />;
  if (prompts.isError) return <ErrorNote error={prompts.error} onRetry={() => void prompts.refetch()} />;
  const kind = prompts.data.kind;
  return (
    <div className="flex min-w-0 flex-col gap-3">
      <div role="group" aria-label="Prompt type" className="flex w-fit rounded-(--radius-control) border border-border p-0.5 text-sm">
        {(["optimized", "custom"] as const).map((k) => (
          <button
            key={k}
            type="button"
            aria-pressed={kind === k}
            onClick={() => kind !== k && setAsk(k)}
            className={`rounded px-3 py-1 ${kind === k ? "bg-accent-soft font-medium" : ""}`}
          >
            {k === "optimized" ? "Optimized prompt" : "Custom prompt"}
          </button>
        ))}
      </div>
      {kind === "optimized" ? <OptimizedPanel campaign={campaign} prompts={prompts.data} ws={ws} /> : custom}
      <Modal
        open={!!ask}
        onClose={() => setAsk(null)}
        title={ask === "optimized" ? "Switch to the optimized prompt?" : "Switch to a custom prompt?"}
        footer={
          <>
            <Button variant="secondary" onClick={() => setAsk(null)}>
              Cancel
            </Button>
            <Button busy={change.isPending} onClick={() => ask && change.mutate(ask)}>
              Switch
            </Button>
          </>
        }
      >
        <p className="text-sm">
          {ask === "optimized"
            ? "The server will build the user prompt and the default prompt blocks will be copied in. Your custom prompts stay saved. Emails not sent yet are marked out of date."
            : "Your own system and user prompts are used again. The optimized checks stop. Emails not sent yet are marked out of date."}
        </p>
      </Modal>
    </div>
  );
}

const AREA = "w-full rounded-(--radius-control) border border-border bg-surface-2 p-3 font-mono text-[13px] leading-relaxed";

function OptimizedPanel({ campaign, prompts, ws }: { campaign: S["CampaignDetail"]; prompts: S["CampaignPrompts"]; ws: string }) {
  const qc = useQueryClient();
  const [tab, setTab] = useState<Tab>("primary");
  const [text, setText] = useState<Record<Block, string>>({ core: prompts.core.text, primary: prompts.primary.text, followup: prompts.followup.text });
  const [gen, setGen] = useState(false);
  const [compare, setCompare] = useState(false);
  useEffect(() => {
    setText({ core: prompts.core.text, primary: prompts.primary.text, followup: prompts.followup.text });
  }, [prompts]);
  const dirty = (["core", "primary", "followup"] as Block[]).filter((b) => text[b] !== prompts[b].text);
  const refresh = () => qc.invalidateQueries({ queryKey: ["campaigns", campaign.id] });
  const save = useMutation({
    mutationFn: () =>
      http.put<S["CampaignPrompts"]>("/v1/campaigns/{id}/prompts", {
        path: { id: campaign.id },
        body: Object.fromEntries(dirty.map((b) => [b, text[b]])),
      }),
    onSuccess: async () => {
      await refresh();
      toast.success("Saved for this campaign.");
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const reset = useMutation({
    mutationFn: (block: Block) => http.post<S["CampaignPrompts"]>("/v1/campaigns/{id}/prompts/reset", { path: { id: campaign.id }, body: { block } }),
    onSuccess: async () => {
      await refresh();
      toast.success("Back to the default.");
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const own: Block = tab;
  const ownLabel = tab === "primary" ? "Primary mail block (step 1)" : "Follow-up block (step 2 and later)";
  const coreBroken = !text.core.includes("# OUTPUT") || !text.core.includes("# HOLD RULE");

  return (
    <section aria-label="Optimized prompt" className="flex min-w-0 flex-col gap-3">
      <RunProgress campaignId={campaign.id} />
      {!prompts.has_profile && (
        <Notice tone="warning">
          Mails will be HOLD until you fill your{" "}
          <Link to="/w/$ws/personalization" params={{ ws }} className="underline">
            PERSONALIZATION page
          </Link>
          .
        </Notice>
      )}
      {coreBroken && <Notice tone="warning">The checks need the sections # OUTPUT and # HOLD RULE in Core.</Notice>}
      {prompts.newer_default && (
        <Notice tone="info">
          A newer default prompt exists.{" "}
          <button type="button" className="underline" onClick={() => setCompare(true)}>
            Compare
          </button>
        </Notice>
      )}
      <Tabs
        label="Mail type"
        value={tab}
        onChange={setTab}
        items={[
          { value: "primary", label: "Primary mail" },
          { value: "followup", label: "Follow-ups" },
        ]}
      />

      <div className="flex flex-col gap-1.5">
        <h3 className="text-sm font-medium">System prompt</h3>
        <details className="rounded-(--radius-control) border border-border">
          <summary className="flex cursor-pointer items-center gap-1 px-3 py-2 text-sm">
            <ChevronDown size={14} /> Core (shared by both tabs){prompts.core.is_default ? "" : " - edited"}
          </summary>
          <div className="p-3 pt-0">
            <textarea aria-label="Core block" rows={14} value={text.core} maxLength={16000} onChange={(e) => setText({ ...text, core: e.target.value })} className={AREA} />
            <ResetLink show={!prompts.core.is_default} onClick={() => reset.mutate("core")} />
          </div>
        </details>
        <label className="flex flex-col gap-1.5 text-sm">
          <span className="font-medium">{ownLabel}</span>
          <textarea aria-label={ownLabel} rows={12} value={text[own]} maxLength={16000} onChange={(e) => setText({ ...text, [own]: e.target.value })} className={AREA} />
        </label>
        <ResetLink show={!prompts[own].is_default} onClick={() => reset.mutate(own)} />
        <div className="flex items-center gap-2">
          <Button busy={save.isPending} disabled={!dirty.length} onClick={() => save.mutate()}>
            Save prompt
          </Button>
          {dirty.length > 0 && <span className="text-xs text-muted">Not saved yet</span>}
          <Button variant="secondary" className="ml-auto" onClick={() => setGen(true)}>
            <Sparkles size={14} /> Generate
          </Button>
        </div>
      </div>

      {tab === "followup" && <Delays campaign={campaign} delays={prompts.delays} ws={ws} />}
      <TestBox campaign={campaign} tab={tab} steps={prompts.delays.map((d) => d.step_number)} dirty={dirty.length > 0} />
      <GenerateDialog open={gen} onClose={() => setGen(false)} campaign={campaign} />
      <Modal open={compare} onClose={() => setCompare(false)} size="lg" title="Your copy and the new default">
        <CompareBody prompts={prompts} />
      </Modal>
    </section>
  );
}

function ResetLink({ show, onClick }: { show: boolean; onClick: () => void }) {
  if (!show) return null;
  return (
    <button type="button" className="mt-1 w-fit text-xs underline" onClick={onClick}>
      Reset to default
    </button>
  );
}

function Delays({ campaign, delays, ws }: { campaign: S["CampaignDetail"]; delays: S["StepDelay"][]; ws: string }) {
  return (
    <div className="rounded-(--radius-control) border border-border p-3 text-sm">
      <h3 className="mb-1 font-medium">When each follow-up goes</h3>
      <ul className="mb-1 text-muted">
        {delays
          .filter((d) => d.step_number > 1)
          .map((d) => (
            <li key={d.step_number}>
              {stepName(d.step_number)}: {d.delay_amount} {d.delay_type} after the step before
            </li>
          ))}
      </ul>
      <Link to="/w/$ws/campaigns/$id/followups" params={{ ws, id: campaign.id }} className="text-xs underline">
        Change this in the Follow-ups tab
      </Link>
    </div>
  );
}

function CompareBody({ prompts }: { prompts: S["CampaignPrompts"] }) {
  const d = useQuery({ queryKey: ["prompts", "defaults"], queryFn: () => http.get<S["Defaults"]>("/v1/prompts/defaults") });
  if (!d.data) return <Skeleton rows={4} />;
  return (
    <div className="flex flex-col gap-4 text-sm">
      <p className="text-muted">
        Your copy is from version {prompts.version ?? "?"}. The shipped default is version {d.data.version}. Nothing changes by itself. Use "Reset to default" on a block to take the new text.
      </p>
      {(["core", "primary", "followup"] as Block[]).map((b) => (
        <div key={b} className="grid gap-2 md:grid-cols-2">
          <div>
            <h4 className="mb-1 font-medium">Your {b}</h4>
            <pre className="max-h-64 overflow-auto whitespace-pre-wrap rounded bg-surface-2 p-2 text-xs">{prompts[b].text}</pre>
          </div>
          <div>
            <h4 className="mb-1 font-medium">New default {b}</h4>
            <pre className="max-h-64 overflow-auto whitespace-pre-wrap rounded bg-surface-2 p-2 text-xs">{d.data[b]}</pre>
          </div>
        </div>
      ))}
    </div>
  );
}

/** The user prompt the server builds, and a test on one lead. */
function TestBox({ campaign, tab, steps, dirty }: { campaign: S["CampaignDetail"]; tab: Tab; steps: number[]; dirty: boolean }) {
  const leads = useQuery({
    queryKey: ["campaigns", campaign.id, "test-leads"],
    queryFn: () => http.get<S["CampaignLeadsReply"]>("/v1/campaigns/{id}/leads", { path: { id: campaign.id }, query: { limit: 50 } }),
  });
  const options = tab === "primary" ? [1] : steps.filter((s) => s >= 2);
  const [lead, setLead] = useState("");
  const [step, setStep] = useState(options[0] ?? 1);
  const [angles, setAngles] = useState("");
  useEffect(() => setStep(options[0] ?? 1), [tab]); // eslint-disable-line react-hooks/exhaustive-deps
  const leadId = lead || (leads.data?.leads[0]?.lead_id ?? "");
  const body = (dry: boolean) => ({
    lead_id: leadId,
    step,
    dry_run: dry,
    ...(step > 1 && angles.trim() ? { previous_angles: angles.split("\n").map((a) => a.trim()).filter(Boolean) } : {}),
  });
  const built = useQuery({
    queryKey: ["campaigns", campaign.id, "user-prompt", leadId, step, angles],
    enabled: !!leadId,
    queryFn: () => http.post<S["OptimizedPreviewReply"]>("/v1/campaigns/{id}/preview", { path: { id: campaign.id }, body: body(true) }),
  });
  const run = useMutation({
    mutationFn: () => http.post<S["OptimizedPreviewReply"]>("/v1/campaigns/{id}/preview", { path: { id: campaign.id }, body: body(false) }),
  });
  const r = run.data;
  return (
    <div className="flex flex-col gap-3">
      <div>
        <h3 className="mb-1 text-sm font-medium">User prompt (built by the server, read only)</h3>
        <pre aria-label="User prompt" className="max-h-64 overflow-auto whitespace-pre-wrap rounded-(--radius-control) border border-border bg-surface-2 p-3 text-xs">
          {built.data?.user ?? (leadId ? "..." : "Add a lead to this campaign to see it.")}
        </pre>
      </div>
      <div className="rounded-(--radius-control) border border-border p-3">
        <h3 className="mb-2 text-sm font-medium">Test</h3>
        {dirty && <Notice tone="info">Save the prompt first. The test uses the saved prompt.</Notice>}
        <div className="grid gap-3 sm:grid-cols-2">
          <Select label="Lead" value={leadId} onChange={setLead} options={(leads.data?.leads ?? []).map((l) => ({ value: l.lead_id, label: l.email }))} />
          <Select label="Step" value={String(step)} onChange={(v) => setStep(Number(v))} options={options.map((s) => ({ value: String(s), label: stepName(s) }))} />
        </div>
        {step > 1 && (
          <TextArea
            label="Earlier angles"
            rows={2}
            value={angles}
            onChange={(e) => setAngles(e.target.value)}
            help={`One per line. Leave empty to use the saved emails of this lead. ${built.data?.previous_angles.length ? `Now: ${built.data.previous_angles.join("; ")}` : "None saved yet."}`}
          />
        )}
        <div className="mt-2">
          <Button busy={run.isPending} disabled={!leadId || dirty} onClick={() => run.mutate()}>
            Generate preview
          </Button>
        </div>
        {run.isError && <ErrorNote error={run.error} />}
        {r && <PreviewResult r={r} />}
      </div>
    </div>
  );
}

function PreviewResult({ r }: { r: S["OptimizedPreviewReply"] }) {
  const json = { status: r.status, subject: r.subject, body: r.body, angle_used: r.angle_used, used_trigger: r.used_trigger, claims_check: r.claims_check, hold_reason: r.held?.reason ?? "" };
  return (
    <div className="mt-3 flex flex-col gap-3" data-testid="preview-result">
      <div className="flex flex-wrap items-center gap-2 text-sm">
        <Pill tone={r.held ? "danger" : "success"}>{r.held ? "HOLD" : "OK"}</Pill>
        <span>{r.word_count} words</span>
        <span className="text-muted">
          {r.calls} call{r.calls === 1 ? "" : "s"}, {r.tokens_in + r.tokens_out} tokens
        </span>
      </div>
      {r.held && <Notice tone="warning">Held: {r.held.reason}</Notice>}
      <pre className="max-h-64 overflow-auto whitespace-pre-wrap rounded bg-surface-2 p-3 text-xs">{JSON.stringify(json, null, 2)}</pre>
      <ul className="grid gap-1 text-sm sm:grid-cols-2" aria-label="Checks">
        {r.checks.map((c) => (
          <li key={c.name} className="flex items-start gap-1.5">
            {c.passed ? <CheckCircle2 size={14} className="mt-0.5 text-success" /> : <XCircle size={14} className="mt-0.5 text-danger" />}
            <span>
              {c.name.replaceAll("_", " ")}
              {c.passed ? "" : `: ${c.detail}`}
            </span>
          </li>
        ))}
      </ul>
    </div>
  );
}
