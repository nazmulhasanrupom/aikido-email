import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { ArrowLeft } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { Button, ConfirmDialog, EmptyState, ErrorNote, Modal, Notice, Page, PageHeader, Skeleton } from "@/components/ui";
import { useUser } from "@/features/settings/useUser";
import { fmtAgo } from "@/lib/format";
import { aq, call } from "./api";
import { CONTROL } from "./ParamForm";
import type { Obj, SecretRow } from "./types";

/** The title with a back arrow in front. It is in the header, so the sticky header of the Aikido design cannot hide it. */
export function Titled({ ws, children }: { ws: string; children: string }) {
  return (
    <span className="flex items-center gap-2">
      <Link
        to="/w/$ws/automations"
        params={{ ws }}
        aria-label="Back to Automations"
        title="Back to Automations"
        className="flex size-8 shrink-0 items-center justify-center rounded-(--radius-control) text-muted hover:bg-surface-2 hover:text-text"
      >
        <ArrowLeft size={18} aria-hidden="true" />
      </Link>
      {children}
    </span>
  );
}

// ---------- secrets ----------

const NAME_RE = /^[A-Z][A-Z0-9_]{0,63}$/;

export function SecretsPage({ ws }: { ws: string }) {
  const { isAdmin } = useUser();
  const client = useQueryClient();
  const secrets = useQuery(aq.secrets);
  const [editing, setEditing] = useState<{ name: string; isNew: boolean } | null>(null);
  const [del, setDel] = useState<SecretRow | null>(null);
  const refresh = () => client.invalidateQueries({ queryKey: ["automations", "secrets"] });
  return (
    <Page>
      <PageHeader
        title={<Titled ws={ws}>Secrets</Titled>}
        sub="Keys for outside services. A workflow reads one as ${{ secret('NAME') }}, or picks it in the auth settings of HTTP Request."
        actions={isAdmin && <Button onClick={() => setEditing({ name: "", isNew: true })}>Add a secret</Button>}
      />
      {!isAdmin && <Notice tone="info">Only an owner or admin can add or delete secrets. You can use the names.</Notice>}
      {secrets.isPending && <Skeleton rows={3} />}
      {secrets.isError && <ErrorNote error={secrets.error} onRetry={() => secrets.refetch()} />}
      {secrets.data?.length === 0 && <EmptyState title="No secrets" text="Add the API key of a service, then use its name in a workflow. The value is stored sealed and never shown again." />}
      {secrets.data && secrets.data.length > 0 && (
        <div className="mt-3 overflow-x-auto rounded-(--radius-card) border border-border bg-surface">
          <table className="w-full border-collapse text-sm">
            <thead>
              <tr className="border-b border-border">
                {["Name", "Value", "Updated", ""].map((h, i) => (
                  <th key={i} scope="col" className="px-4 py-3 text-left text-[11px] font-semibold uppercase tracking-wider text-muted">
                    {h}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {secrets.data.map((s) => (
                <tr key={s.name} className="border-t border-border first:border-t-0">
                  <td className="px-4 py-3 font-mono text-[13px]">{s.name}</td>
                  <td className="px-4 py-3 font-mono text-[13px] text-muted">••••{s.last4}</td>
                  <td className="px-4 py-3 text-muted">{fmtAgo(s.updated_at)}</td>
                  <td className="px-4 py-3 text-right">
                    {isAdmin && (
                      <span className="inline-flex gap-1">
                        <Button variant="secondary" className="h-8" onClick={() => setEditing({ name: s.name, isNew: false })}>
                          Replace
                        </Button>
                        <Button variant="ghost" className="h-8" onClick={() => setDel(s)}>
                          Delete
                        </Button>
                      </span>
                    )}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      {editing && <SecretForm key={editing.name || "new"} {...editing} onClose={() => setEditing(null)} onSaved={() => { setEditing(null); void refresh(); }} />}
      <ConfirmDialog
        open={del !== null}
        onClose={() => setDel(null)}
        title="Delete this secret?"
        text={del ? `Workflows that use ${del.name} will show unknown_secret and fail until you add it again.` : ""}
        confirmLabel="Delete"
        onConfirm={async () => {
          if (!del) return;
          await call.deleteSecret(del.name);
          toast.success("Deleted.");
          setDel(null);
          void refresh();
        }}
      />
    </Page>
  );
}

function SecretForm({ name: initial, isNew, onClose, onSaved }: { name: string; isNew: boolean; onClose: () => void; onSaved: () => void }) {
  const [name, setName] = useState(initial);
  const [value, setValue] = useState("");
  const save = useMutation({
    mutationFn: () => call.saveSecret(name, value),
    onSuccess: () => {
      toast.success("Saved.");
      onSaved();
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const nameOk = NAME_RE.test(name);
  const valueOk = value.length >= 4 && value.length <= 10000;
  return (
    <Modal
      open
      onClose={onClose}
      title={isNew ? "Add a secret" : `Replace ${initial}`}
      size="sm"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button busy={save.isPending} disabled={!nameOk || !valueOk} onClick={() => save.mutate()}>
            Save
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        <label className="flex flex-col gap-1 text-sm">
          <span className="font-medium">Name</span>
          <input value={name} disabled={!isNew} onChange={(e) => setName(e.target.value.toUpperCase())} placeholder="SEARCH_API_KEY" className={`${CONTROL} font-mono text-[13px]`} />
          <span className="text-xs text-muted">Capital letters, digits and _. Start with a letter.</span>
        </label>
        <label className="flex flex-col gap-1 text-sm">
          <span className="font-medium">Value</span>
          <input type="password" autoComplete="off" value={value} onChange={(e) => setValue(e.target.value)} className={CONTROL} />
          <span className="text-xs text-muted">4 to 10000 characters. It is sealed with AES-256-GCM and is not shown again.</span>
        </label>
      </div>
    </Modal>
  );
}

// ---------- settings ----------

interface Settings extends Obj {
  allow_private_network: boolean;
  allow_lead_erase: boolean;
  fetch_respect_robots: boolean;
  fetch_blocked_domains: string[];
  retention_days: number;
  test_runs_kept: number;
  webhook_rate_per_min: number;
  event_tick_secs: number;
  ai_budget_wait_hours: number;
  max_parallel_runs: number;
}

const NUMBERS: [keyof Settings, string, number, number, string][] = [
  ["retention_days", "Keep production runs (days)", 1, 365, "Older runs are deleted every hour."],
  ["test_runs_kept", "Test runs kept per workflow", 1, 100, "The newest ones. None older than 7 days."],
  ["webhook_rate_per_min", "Webhook requests per minute", 1, 6000, "Per webhook path."],
  ["event_tick_secs", "Event check every (seconds)", 1, 300, "How often event triggers look for new events."],
  ["ai_budget_wait_hours", "Wait for the AI budget (hours)", 0, 168, "A run waits this long for the daily token budget, then fails."],
  ["max_parallel_runs", "Runs at the same time per server copy", 1, 100, "The environment variable AIKIDO_AUTOMATIONS_MAX_PARALLEL wins if it is set."],
];

export function SettingsPage({ ws }: { ws: string }) {
  const { isAdmin } = useUser();
  const client = useQueryClient();
  const settings = useQuery(aq.settings);
  const status = useQuery(aq.status);
  const [form, setForm] = useState<Settings | null>(null);
  useEffect(() => {
    if (settings.data && !form) setForm(settings.data as Settings);
  }, [settings.data]);
  const save = useMutation({
    mutationFn: () => {
      const orig = settings.data as Settings;
      const patch: Obj = {};
      for (const k of Object.keys(form ?? {})) if (JSON.stringify((form as Settings)[k]) !== JSON.stringify(orig[k])) patch[k] = (form as Settings)[k];
      return call.saveSettings(patch);
    },
    onSuccess: (r) => {
      toast.success("Saved.");
      setForm(r as Settings);
      void client.invalidateQueries({ queryKey: ["automations", "settings"] });
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const pause = useMutation({
    mutationFn: () => (status.data?.paused ? call.resume() : call.pause()),
    onSuccess: () => void client.invalidateQueries({ queryKey: ["automations", "status"] }),
    onError: (e) => toast.error(messageOf(e)),
  });
  const set = <K extends keyof Settings>(k: K, v: Settings[K]) => setForm((f) => (f ? { ...f, [k]: v } : f));
  const dirty = !!form && !!settings.data && JSON.stringify(form) !== JSON.stringify(settings.data);
  return (
    <Page>
      <PageHeader title={<Titled ws={ws}>Automation settings</Titled>} actions={isAdmin && <Button busy={save.isPending} disabled={!dirty} onClick={() => save.mutate()}>Save</Button>} />
      {!isAdmin && <Notice tone="info">Only an owner or admin can change these settings.</Notice>}
      {settings.isPending && <Skeleton rows={6} />}
      {settings.isError && <ErrorNote error={settings.error} onRetry={() => settings.refetch()} />}
      {form && (
        <div className="mt-3 flex max-w-2xl flex-col gap-6">
          <section className="flex flex-col gap-3 rounded-(--radius-card) border border-border bg-surface p-4" aria-label="Limits">
            <h2 className="font-semibold">Limits</h2>
            <div className="grid gap-3 sm:grid-cols-2">
              {NUMBERS.map(([k, label, min, max, help]) => (
                <label key={k} className="flex flex-col gap-1 text-sm">
                  <span className="font-medium">{label}</span>
                  <input type="number" min={min} max={max} disabled={!isAdmin} value={form[k] as number} onChange={(e) => set(k, Number(e.target.value) as never)} className={CONTROL} />
                  <span className="text-xs text-muted">{help}</span>
                </label>
              ))}
            </div>
          </section>
          <section className="flex flex-col gap-2 rounded-(--radius-card) border border-border bg-surface p-4" aria-label="Engine">
            <h2 className="font-semibold">Engine</h2>
            {status.data && (
              <p className="text-sm text-muted">
                {status.data.paused ? "Paused: production runs wait. Test runs still run." : "Running."} {status.data.active_workflows} active workflow{status.data.active_workflows === 1 ? "" : "s"}.
                {status.data.worker_enabled ? "" : " This server copy does not run automations (AIKIDO_AUTOMATIONS_ENABLED is off)."}
              </p>
            )}
            {(status.data?.warnings ?? []).map((w) => (
              <Notice key={w} tone="warning">
                {w}
              </Notice>
            ))}
            {isAdmin && (
              <Button variant="secondary" className="w-fit" busy={pause.isPending} onClick={() => pause.mutate()}>
                {status.data?.paused ? "Resume the engine" : "Pause the engine"}
              </Button>
            )}
          </section>
        </div>
      )}
    </Page>
  );
}
