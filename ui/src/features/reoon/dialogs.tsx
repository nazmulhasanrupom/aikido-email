import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { ApiProblem, messageOf } from "@/api/errors";
import { http } from "@/api/http";
import { PasswordField } from "@/components/PasswordField";
import { Button, Check2, ErrorNote, Modal, NumField, Pill, Select, Table, TD, Head } from "@/components/ui";
import { fmtAgo, fmtInt } from "@/lib/format";
import { PROBLEM_WORDS, say } from "@/lib/words";
import {
  type ReoonSettings,
  type Run,
  type SendRule,
  STATUSES,
  appsQuery,
  reoonSettingsQuery,
} from "./api";

const KEY_PAGE = "https://emailverifier.reoon.com/api-settings/";

/** Words for an API problem. The code is mapped first, so the text is the one the build map asks for. */
export function problemText(e: unknown): string {
  return e instanceof ApiProblem ? (PROBLEM_WORDS[e.code] ?? e.message) : messageOf(e);
}

export function InstallDialog({ open, onClose }: { open: boolean; onClose: () => void }) {
  const qc = useQueryClient();
  const [key, setKey] = useState("");
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (open) {
      setKey("");
      setError(null);
    }
  }, [open]);
  const install = useMutation({
    mutationFn: () => http.post("/v1/integrations/apps/reoon/install", { body: { api_key: key } }),
    onSuccess: async () => {
      await qc.invalidateQueries({ queryKey: appsQuery.queryKey });
      toast.success("Reoon installed. Find it in the menu.");
      onClose();
    },
    onError: (e) => setError(problemText(e)),
  });
  return (
    <Modal
      open={open}
      onClose={onClose}
      size="sm"
      title="Install Reoon Email Verifier"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button busy={install.isPending} disabled={!key.trim()} onClick={() => install.mutate()}>
            Install
          </Button>
        </>
      }
    >
      <form
        className="flex flex-col gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          if (key.trim()) install.mutate();
        }}
      >
        <PasswordField
          label="API key"
          value={key}
          autoComplete="off"
          onChange={(e) => {
            setKey(e.target.value);
            setError(null);
          }}
          error={error ?? undefined}
        />
        <a href={KEY_PAGE} target="_blank" rel="noreferrer" className="text-sm text-accent underline">
          Where do I find my key?
        </a>
        <p className="text-xs text-muted">
          We test the key first. It is saved encrypted and is never shown again.
        </p>
      </form>
    </Modal>
  );
}

/** Choose which statuses may start a campaign. Locked statuses are off and can not be turned on. */
export function RuleDialog({
  open,
  onClose,
  rule,
  onSave,
  title = "Send rule",
}: {
  open: boolean;
  onClose: () => void;
  rule: SendRule;
  onSave: (r: SendRule) => Promise<unknown>;
  title?: string;
}) {
  const [allowed, setAllowed] = useState<Set<string>>(new Set(rule.allowed));
  const [notVerified, setNotVerified] = useState(rule.allow_not_verified);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    if (open) {
      setAllowed(new Set(rule.allowed));
      setNotVerified(rule.allow_not_verified);
      setError(null);
    }
  }, [open, rule]);
  async function save() {
    setBusy(true);
    try {
      await onSave({ allowed: [...allowed], allow_not_verified: notVerified });
      onClose();
    } catch (e) {
      setError(problemText(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <Modal
      open={open}
      onClose={onClose}
      size="sm"
      title={title}
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button busy={busy} onClick={save}>
            Save
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        <p className="text-sm text-muted">
          Only leads with these results can start a campaign. Leads already in a sequence keep going.
        </p>
        {STATUSES.map((s) => (
          <Check2
            key={s.code}
            label={say("reoon", s.code)}
            help={s.locked ? `Never allowed. ${s.locked}` : undefined}
            disabled={!!s.locked}
            checked={!s.locked && allowed.has(s.code)}
            onChange={(v) => {
              const n = new Set(allowed);
              if (v) n.add(s.code);
              else n.delete(s.code);
              setAllowed(n);
            }}
          />
        ))}
        <Check2 label="Not verified" help="Leads with no result yet." checked={notVerified} onChange={setNotVerified} />
        {error && <ErrorNote error={error} />}
      </div>
    </Modal>
  );
}

const MODES = [
  { value: "power", label: "Power: checks that the inbox exists (slow, up to a minute each)" },
  { value: "quick", label: "Quick: under a second, but does not check the inbox" },
];

export function patchSettings(patch: Partial<ReoonSettings>) {
  return http.patch<{ settings: ReoonSettings }>("/v1/verify/reoon/settings", { body: patch });
}

/** The gear: mode, speed, saved results, the toggle for verify on import, a new key, and the run history. */
export function SettingsDialog({ open, onClose, isAdmin }: { open: boolean; onClose: () => void; isAdmin: boolean }) {
  const qc = useQueryClient();
  const settings = useQuery({ ...reoonSettingsQuery, enabled: open });
  const runs = useQuery({
    queryKey: ["reoon", "runs"],
    enabled: open,
    queryFn: async () => (await http.get<{ runs: Run[] }>("/v1/verify/reoon/runs")).runs,
  });
  const s = settings.data?.settings;
  const [concurrency, setConcurrency] = useState(5);
  const [reuse, setReuse] = useState(30);
  const [key, setKey] = useState("");
  useEffect(() => {
    if (s) {
      setConcurrency(s.concurrency);
      setReuse(s.reuse_days);
    }
  }, [s]);
  const save = useMutation({
    mutationFn: (patch: Partial<ReoonSettings>) => patchSettings(patch),
    onSuccess: () => qc.invalidateQueries({ queryKey: reoonSettingsQuery.queryKey }),
    onError: (e) => toast.error(problemText(e)),
  });
  const newKey = useMutation({
    mutationFn: () => http.patch("/v1/integrations/apps/reoon", { body: { api_key: key } }),
    onSuccess: async () => {
      setKey("");
      toast.success("Key updated.");
      await qc.invalidateQueries({ queryKey: appsQuery.queryKey });
    },
    onError: (e) => toast.error(problemText(e)),
  });
  return (
    <Modal open={open} onClose={onClose} size="lg" title="Reoon settings" footer={<Button onClick={onClose}>Close</Button>}>
      {settings.isError && <ErrorNote error={settings.error} />}
      {s && (
        <div className="flex flex-col gap-5">
          <Check2
            label="Verify new leads automatically"
            help="New leads from a CSV import, the API or a manual add are checked at once. Leads with a recent result cost nothing."
            checked={s.auto_verify}
            onChange={(v) => save.mutate({ auto_verify: v })}
          />
          <Select label="Mode" value={s.mode} options={MODES} onChange={(v) => save.mutate({ mode: v as "power" | "quick" })} />
          <div className="grid gap-3 sm:grid-cols-2">
            <div onBlur={() => concurrency !== s.concurrency && save.mutate({ concurrency })}>
              <NumField label="Emails checked at the same time (1 to 20)" value={concurrency} min={1} max={20} onChange={setConcurrency} />
            </div>
            <div onBlur={() => reuse !== s.reuse_days && save.mutate({ reuse_days: reuse })}>
              <NumField label="Keep a result for this many days" value={reuse} min={0} max={365} onChange={setReuse} />
            </div>
          </div>
          {isAdmin && (
            <form
              className="flex items-end gap-2"
              onSubmit={(e) => {
                e.preventDefault();
                if (key.trim()) newKey.mutate();
              }}
            >
              <div className="flex-1">
                <PasswordField label="Replace the API key" value={key} autoComplete="off" onChange={(e) => setKey(e.target.value)} />
              </div>
              <Button type="submit" variant="secondary" busy={newKey.isPending} disabled={!key.trim()}>
                Update key
              </Button>
            </form>
          )}
          <p className="text-xs text-muted">
            Emails that Reoon calls invalid, disabled, disposable or a spam trap are only marked. To block them, select them on the Reoon page and click Block these.
          </p>
          <section>
            <h3 className="mb-2 text-sm font-semibold">Last runs</h3>
            {runs.data?.length === 0 && <p className="text-sm text-muted">No run yet.</p>}
            {runs.data && runs.data.length > 0 && (
              <Table>
                <Head cols={["Started", "Mode", "Checked", "Reused", "Failed", "State"]} />
                <tbody>
                  {runs.data.slice(0, 20).map((r) => (
                    <tr key={r.id} className="border-t border-border">
                      <td className={TD}>
                        {fmtAgo(r.created_at)}
                        {r.auto ? " (new leads)" : ""}
                      </td>
                      <td className={TD}>{r.mode === "power" ? "Power" : "Quick"}</td>
                      <td className={TD}>{fmtInt(r.done_count)} of {fmtInt(r.total)}</td>
                      <td className={TD}>{fmtInt(r.skipped_count)}</td>
                      <td className={TD}>{fmtInt(r.failed_count)}</td>
                      <td className={TD}>
                        <Pill tone={r.status === "done" ? "success" : r.status === "failed" ? "danger" : "neutral"}>
                          {r.error ? (PROBLEM_WORDS[r.error] ? r.error.replaceAll("_", " ") : r.status) : r.status}
                        </Pill>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </Table>
            )}
          </section>
        </div>
      )}
    </Modal>
  );
}
