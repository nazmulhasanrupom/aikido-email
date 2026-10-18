import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useParams } from "@tanstack/react-router";
import { Braces, Sparkles } from "lucide-react";
import { useRef, useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import { MenuContent, MenuItem, MenuLabel, MenuRoot, MenuSeparator, MenuTrigger } from "@/components/shell/menu";
import { Button, ErrorNote, Modal, Notice, Select, Skeleton } from "@/components/ui";
import { fmtInt } from "@/lib/format";
import { GenerateDialog, RunProgress } from "./AiTools";
import { stepName } from "./EmailDrawer";
import { ModeSwitch } from "./ModeSwitch";

const AREA = "w-full rounded-(--radius-control) border border-border bg-surface-2 p-3 text-sm leading-relaxed";

const defaultsQuery = { queryKey: ["prompts", "defaults"], queryFn: () => http.get<S["PromptDefaults"]>("/v1/prompts/defaults"), staleTime: 300_000 };
const profileQuery = { queryKey: ["personalization", "profile"], queryFn: () => http.get<S["ProfileReply"]>("/v1/personalization/profile") };

/** The Optimised mode: one prompt writes the primary email and 3 follow-ups for each lead. */
export function OptimizedEditor({ campaign }: { campaign: S["CampaignDetail"] }) {
  const { ws } = useParams({ strict: false }) as { ws: string };
  const qc = useQueryClient();
  const defaults = useQuery(defaultsQuery);
  const profile = useQuery(profileQuery);
  const gen = useQuery(q.genSettings);
  const providers = useQuery(q.providers);
  // "" means the shipped default
  const saved = { system: campaign.sequence.opt_system_prompt ?? "", user: campaign.sequence.opt_user_prompt ?? "" };
  const [system, setSystem] = useState<string | null>(null);
  const [user, setUser] = useState<string | null>(null);
  const [genOpen, setGenOpen] = useState(false);
  const [test, setTest] = useState(false);
  const userRef = useRef<HTMLTextAreaElement>(null);
  const shown = {
    system: system ?? (saved.system || defaults.data?.system || ""),
    user: user ?? (saved.user || defaults.data?.user || ""),
  };
  const dirty = (system !== null && system !== (saved.system || defaults.data?.system)) || (user !== null && user !== (saved.user || defaults.data?.user));

  const save = useMutation({
    mutationFn: async () => {
      // text equal to the default is saved as empty, so the campaign follows the shipped default
      const asSaved = (v: string, d: string | undefined) => (v.trim() === (d ?? "").trim() ? "" : v);
      const sys = asSaved(shown.system, defaults.data?.system);
      const usr = asSaved(shown.user, defaults.data?.user);
      await http.patch("/v1/followups/sequences/{id}", { path: { id: campaign.sequence.id }, body: { opt_system_prompt: sys, opt_user_prompt: usr } });
    },
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: ["campaigns", campaign.id] });
      setSystem(null);
      setUser(null);
      toast.success("Saved. Use Generate to write the emails.");
    },
    onError: (e) => toast.error(messageOf(e)),
  });

  const slotProvider = gen.data?.settings.slot_provider_id;
  const aiReady = !!slotProvider && !!providers.data?.some((p) => p.id === slotProvider && p.status === "ok");
  function insert(name: string) {
    const el = userRef.current;
    const at = el?.selectionStart ?? shown.user.length;
    const text = `{{${name}}}`;
    setUser(shown.user.slice(0, at) + text + shown.user.slice(at));
    requestAnimationFrame(() => el?.focus());
  }
  if (defaults.isPending) return <Skeleton rows={8} />;
  if (defaults.isError) return <ErrorNote error={defaults.error} onRetry={() => void defaults.refetch()} />;

  return (
    <div className="mx-auto flex max-w-3xl flex-col gap-5">
      <div className="flex flex-wrap items-center gap-3">
        <ModeSwitch campaign={campaign} />
        <Button className="ml-auto" busy={save.isPending} disabled={!dirty} onClick={() => save.mutate()}>
          Save
        </Button>
      </div>
      <RunProgress campaignId={campaign.id} />
      {!aiReady && <Notice tone="warning">Set up an AI model first (Settings, AI). Without it nothing can be written.</Notice>}
      {profile.data && !profile.data.profile && (
        <Notice tone="info">
          The AI does not know your offer yet. Fill the{" "}
          <Link to="/w/$ws/personalization" params={{ ws }} className="underline">
            Personalisation page
          </Link>{" "}
          once.
        </Notice>
      )}

      <section aria-label="Instructions" className="flex flex-col gap-1.5">
        <label htmlFor="opt-system" className="text-sm font-medium">
          Instructions for the AI
        </label>
        <textarea id="opt-system" rows={12} value={shown.system} maxLength={8000} onChange={(e) => setSystem(e.target.value)} className={AREA} />
        <Reset show={shown.system.trim() !== (defaults.data?.system ?? "").trim()} onClick={() => setSystem(defaults.data?.system ?? "")} />
      </section>

      <section aria-label="Lead information" className="flex flex-col gap-1.5">
        <div className="flex items-center justify-between">
          <label htmlFor="opt-user" className="text-sm font-medium">
            What the AI knows about you and each lead
          </label>
          <MenuRoot>
            <MenuTrigger className="inline-flex h-8 items-center gap-1.5 rounded-(--radius-control) border border-border bg-surface px-2.5 text-xs font-medium hover:bg-surface-2">
              <Braces size={13} /> Variable
            </MenuTrigger>
            <MenuContent align="end" className="max-h-80 overflow-auto">
              <MenuLabel>About you (Personalisation)</MenuLabel>
              {defaults.data.variables.slice(0, 9).map((v) => (
                <MenuItem key={v} onSelect={() => insert(v)}>
                  {v}
                </MenuItem>
              ))}
              <MenuSeparator />
              <MenuLabel>About the lead</MenuLabel>
              {defaults.data.variables.slice(9).map((v) => (
                <MenuItem key={v} onSelect={() => insert(v)}>
                  {v}
                </MenuItem>
              ))}
            </MenuContent>
          </MenuRoot>
        </div>
        <textarea id="opt-user" ref={userRef} rows={16} value={shown.user} maxLength={8000} onChange={(e) => setUser(e.target.value)} className={AREA} />
        <p className="text-xs text-muted">A line with an empty value is left out. Use {"{{name|fallback}}"} to give a value when it is empty.</p>
        <Reset show={shown.user.trim() !== (defaults.data?.user ?? "").trim()} onClick={() => setUser(defaults.data?.user ?? "")} />
      </section>

      <section aria-label="The 4 emails" className="rounded-(--radius-control) border border-border p-3 text-sm">
        <h2 className="mb-1 font-medium">The 4 emails</h2>
        <ol className="mb-2 flex flex-col gap-0.5 text-muted">
          {campaign.steps.map((s) => (
            <li key={s.step_number}>
              {stepName(s.step_number)}
              {s.step_number > 1 ? `: ${s.delay_amount} ${s.delay_type === "business_days" ? "business " : ""}days after the one before` : ": goes first"}
            </li>
          ))}
        </ol>
        <p className="text-xs text-muted">
          The AI writes all 4 in one go, for each lead. Open a lead to edit its emails.{" "}
          <Link to="/w/$ws/campaigns/$id/followups" params={{ ws, id: campaign.id }} className="underline">
            Change the timing
          </Link>
        </p>
      </section>

      <div className="flex gap-2">
        <Button variant="secondary" disabled={!aiReady || dirty} onClick={() => setTest(true)} title={dirty ? "Save first" : undefined}>
          Test on 1 lead
        </Button>
        <Button disabled={!aiReady || dirty} onClick={() => setGenOpen(true)} title={dirty ? "Save first" : undefined}>
          <Sparkles size={14} /> Generate
        </Button>
      </div>
      <GenerateDialog open={genOpen} onClose={() => setGenOpen(false)} campaign={campaign} />
      <TestDialog open={test} onClose={() => setTest(false)} campaign={campaign} />
    </div>
  );
}

function Reset({ show, onClick }: { show: boolean; onClick: () => void }) {
  if (!show) return null;
  return (
    <button type="button" className="w-fit text-xs underline" onClick={onClick}>
      Reset to default
    </button>
  );
}

/** Write the 4 emails for one lead. Nothing is saved. */
function TestDialog({ open, onClose, campaign }: { open: boolean; onClose: () => void; campaign: S["CampaignDetail"] }) {
  const leads = useQuery({
    queryKey: ["campaigns", campaign.id, "test-leads"],
    enabled: open,
    queryFn: () => http.get<S["CampaignLeadsReply"]>("/v1/campaigns/{id}/leads", { path: { id: campaign.id }, query: { limit: 50 } }),
  });
  const [lead, setLead] = useState("");
  const leadId = lead || (leads.data?.leads[0]?.lead_id ?? "");
  const run = useMutation({
    mutationFn: () => http.post<S["EmailTestReply"]>("/v1/campaigns/{id}/emails/test", { path: { id: campaign.id }, body: { lead_id: leadId, step: 1 } }),
  });
  const r = run.data;
  return (
    <Modal
      open={open}
      onClose={onClose}
      size="lg"
      title="Test on 1 lead"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Close
          </Button>
          <Button busy={run.isPending} disabled={!leadId} onClick={() => run.mutate()}>
            Write the 4 emails
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        {leads.data && leads.data.leads.length === 0 && <Notice tone="info">Add a lead to this campaign first.</Notice>}
        <Select label="Lead" value={leadId} onChange={setLead} options={(leads.data?.leads ?? []).map((l) => ({ value: l.lead_id, label: l.email }))} help="Nothing is saved." />
        {run.isError && <ErrorNote error={run.error} />}
        {r && (
          <>
            <p className="text-xs text-muted">{fmtInt(r.tokens_in + r.tokens_out)} tokens</p>
            {r.emails.map((m) => (
              <article key={m.step} className="rounded-(--radius-control) border border-border p-3 text-sm">
                <h3 className="mb-1 text-xs font-medium text-muted">{stepName(m.step)}</h3>
                {m.subject && <p className="font-medium">{m.subject}</p>}
                <p className="whitespace-pre-wrap">{m.body}</p>
              </article>
            ))}
          </>
        )}
      </div>
    </Modal>
  );
}
