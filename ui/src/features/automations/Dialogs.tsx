import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Plus, Trash2 } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { ApiProblem, messageOf } from "@/api/errors";
import { q } from "@/api/queries";
import { Button, Check2, ErrorNote, Modal, Notice, Pill } from "@/components/ui";
import { VALID_KEY } from "@/lib/fieldKey";
import { timezones } from "@/lib/format";
import { aq, call } from "./api";
import { useEditor } from "./ctx";
import { diffDefs, isTrigger, publishSignature } from "./defs";
import { describeTrigger } from "./meta";
import { CONTROL, JsonField } from "./ParamForm";
import { WorkflowSelect } from "./Phase9Widgets";
import type {
  Issue,
  TriggerInfo,
  Validation,
  VarKind,
  WfDef,
  WfVariable,
  WorkflowDetail,
} from "./types";

// ---------- publish ----------

export function PublishDialog({
  open,
  onClose,
  workflowId,
  def,
  published,
  onSelectNode,
  onDone,
}: {
  open: boolean;
  onClose: () => void;
  workflowId: string;
  def: WfDef;
  published: number | null;
  onSelectNode: (id: string) => void;
  onDone: (w: WorkflowDetail) => void;
}) {
  const [note, setNote] = useState("");
  const [fail, setFail] = useState<Validation | null>(null);
  const check = useQuery({
    queryKey: ["automations", "publish-check", workflowId, publishSignature(def), open],
    queryFn: () => call.validate(def, true),
    enabled: open,
    staleTime: 0,
  });
  const prev = useQuery({
    ...aq.version(workflowId, published ?? 0),
    enabled: open && published !== null,
  });
  const diff = useMemo(() => diffDefs(prev.data?.definition ?? null, def), [prev.data, def]);
  const v = fail ?? check.data;
  const publish = useMutation({
    mutationFn: () => call.publish(workflowId, note.trim()),
    onSuccess: (r) => {
      toast.success(`Version ${r.version} is published.`);
      onDone(r.workflow);
      setNote("");
      onClose();
    },
    onError: (e) => {
      if (e instanceof ApiProblem && e.code === "workflow_has_errors") {
        setFail((e.details.validation as Validation | undefined) ?? null);
      } else toast.error(messageOf(e));
    },
  });
  const errors = v?.errors ?? [];
  const same =
    published !== null &&
    !diff.added.length &&
    !diff.removed.length &&
    !diff.changed.length &&
    !diff.edgesAdded &&
    !diff.edgesRemoved &&
    prev.data !== undefined;
  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Publish"
      size="lg"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button
            busy={publish.isPending}
            disabled={!v || errors.length > 0 || check.isFetching}
            onClick={() => publish.mutate()}
          >
            Publish version {(published ?? 0) + 1}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-4">
        <p className="text-sm text-muted">
          Publishing freezes the draft as a new version. Production runs use the published version.
          The draft can go on changing.
        </p>
        {check.isPending && <p className="text-sm text-muted">Checking the workflow…</p>}
        {check.isError && <ErrorNote error={check.error} />}
        {v && errors.length === 0 && (
          <Notice tone="success">
            No errors.{" "}
            {v.warnings.length
              ? `${v.warnings.length} warning${v.warnings.length === 1 ? "" : "s"}.`
              : ""}
          </Notice>
        )}
        {errors.length > 0 && (
          <IssueList
            title="Fix these first"
            issues={errors}
            tone="danger"
            onSelectNode={(id) => {
              onSelectNode(id);
              onClose();
            }}
          />
        )}
        {v && v.warnings.length > 0 && (
          <IssueList
            title="Warnings"
            issues={v.warnings}
            tone="warning"
            onSelectNode={(id) => {
              onSelectNode(id);
              onClose();
            }}
          />
        )}
        <section aria-label="Changes">
          <h3 className="mb-1 text-sm font-semibold">
            {published === null ? "First version" : `Changes since version ${published}`}
          </h3>
          {published === null && (
            <p className="text-sm text-muted">
              {def.nodes.length} nodes and {def.edges.length} lines.
            </p>
          )}
          {published !== null && same && (
            <p className="text-sm text-muted">Nothing changed in the nodes or lines.</p>
          )}
          {published !== null && !same && (
            <ul className="flex flex-col gap-1 text-sm">
              {diff.added.length > 0 && (
                <li>
                  <Pill tone="success">added</Pill> {diff.added.join(", ")}
                </li>
              )}
              {diff.removed.length > 0 && (
                <li>
                  <Pill tone="danger">removed</Pill> {diff.removed.join(", ")}
                </li>
              )}
              {diff.changed.length > 0 && (
                <li>
                  <Pill tone="info">changed</Pill> {diff.changed.join(", ")}
                </li>
              )}
              {(diff.edgesAdded > 0 || diff.edgesRemoved > 0) && (
                <li className="text-muted">
                  Lines: +{diff.edgesAdded} −{diff.edgesRemoved}
                </li>
              )}
            </ul>
          )}
        </section>
        <label className="flex flex-col gap-1 text-sm">
          <span className="font-medium">Note (optional)</span>
          <input
            value={note}
            maxLength={500}
            onChange={(e) => setNote(e.target.value)}
            placeholder="What changed?"
            className={CONTROL}
          />
        </label>
      </div>
    </Modal>
  );
}

function IssueList({
  title,
  issues,
  tone,
  onSelectNode,
}: {
  title: string;
  issues: Issue[];
  tone: "danger" | "warning";
  onSelectNode: (id: string) => void;
}) {
  return (
    <section aria-label={title}>
      <h3
        className={`mb-1 text-sm font-semibold ${tone === "danger" ? "text-danger" : "text-warning"}`}
      >
        {title}
      </h3>
      <ul className="flex max-h-48 flex-col gap-1 overflow-auto text-sm">
        {issues.map((i, n) => (
          <li key={n} className="flex items-baseline gap-2">
            {i.node_id ? (
              <button
                type="button"
                onClick={() => onSelectNode(i.node_id as string)}
                className="shrink-0 rounded bg-surface-2 px-1.5 font-mono text-xs underline-offset-2 hover:underline"
              >
                {i.node_id}
              </button>
            ) : (
              <span className="shrink-0 rounded bg-surface-2 px-1.5 text-xs text-muted">
                workflow
              </span>
            )}
            <span>{i.message}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

// ---------- active ----------

export function ActiveDialog({
  open,
  onClose,
  workflowId,
  mode,
  published,
  onDone,
}: {
  open: boolean;
  onClose: () => void;
  workflowId: string;
  mode: "on" | "off";
  published: number | null;
  onDone: (w: WorkflowDetail, triggers?: TriggerInfo[]) => void;
}) {
  const campaigns = useQuery({ ...q.campaigns, enabled: open && mode === "on" });
  const version = useQuery({
    ...aq.version(workflowId, published ?? 0),
    enabled: open && mode === "on" && published !== null,
  });
  const [hours, setHours] = useState(0);
  const [cancelRuns, setCancelRuns] = useState(false);
  useEffect(() => {
    if (open) {
      setHours(0);
      setCancelRuns(false);
    }
  }, [open]);
  const def = version.data?.definition;
  const triggers = (def?.nodes ?? []).filter((n) => isTrigger(n.type));
  const hasEvents = triggers.some(
    (t) =>
      !["trigger.manual", "trigger.schedule", "trigger.cron", "trigger.webhook"].includes(t.type),
  );
  const run = useMutation({
    mutationFn: async () => {
      if (mode === "on") return call.activate(workflowId, hours);
      return { workflow: await call.deactivate(workflowId, cancelRuns), triggers: undefined };
    },
    onSuccess: (r) => {
      toast.success(mode === "on" ? "The workflow is active." : "The workflow is off.");
      onDone(r.workflow, r.triggers);
      onClose();
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const origin = window.location.origin;
  return (
    <Modal
      open={open}
      onClose={onClose}
      title={mode === "on" ? "Turn on" : "Turn off"}
      size="md"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button
            busy={run.isPending}
            variant={mode === "on" ? "primary" : "danger"}
            onClick={() => run.mutate()}
          >
            {mode === "on" ? "Turn on" : "Turn off"}
          </Button>
        </>
      }
    >
      {mode === "on" ? (
        <div className="flex flex-col gap-3">
          <p className="text-sm text-muted">Version {published} will run on its triggers:</p>
          {version.isPending && <p className="text-sm text-muted">Loading…</p>}
          <ul className="flex flex-col gap-1.5 text-sm">
            {triggers.map((t) => {
              const cid = typeof t.params?.campaign_id === "string" ? t.params.campaign_id : "";
              const camp = campaigns.data?.campaigns.find((c) => c.id === cid)?.name;
              const path = typeof t.params?.path === "string" ? t.params.path : "";
              return (
                <li
                  key={t.id}
                  className="rounded-(--radius-control) border border-border px-3 py-2"
                >
                  <span className="font-medium">{t.name || t.id}</span>
                  <span className="block text-muted">
                    {describeTrigger(t.type, t.params)}
                    {camp ? `: "${camp}"` : ""}
                  </span>
                  {t.type === "trigger.webhook" && path && (
                    <span className="block font-mono text-xs break-all">
                      {origin}/hooks/{path}
                    </span>
                  )}
                </li>
              );
            })}
          </ul>
          {hasEvents && (
            <label className="flex flex-col gap-1 text-sm">
              <span className="font-medium">Also process events from the last … hours</span>
              <input
                type="number"
                min={0}
                max={168}
                value={hours}
                onChange={(e) => setHours(Math.max(0, Math.min(168, Number(e.target.value) || 0)))}
                className={CONTROL}
              />
              <span className="text-xs text-muted">
                0 means only new events. Up to 168 (7 days).
              </span>
            </label>
          )}
        </div>
      ) : (
        <div className="flex flex-col gap-3">
          <p className="text-sm text-muted">
            The triggers stop. Runs that are queued or running go on unless you cancel them.
          </p>
          <Check2
            label="Also cancel queued and running runs"
            checked={cancelRuns}
            onChange={setCancelRuns}
          />
        </div>
      )}
    </Modal>
  );
}

// ---------- values for the variables ----------

const ID_KINDS: VarKind[] = ["campaign", "list", "account", "provider", "custom_field"];

export function VarInput({ v, onChange }: { v: WfVariable; onChange: (val: unknown) => void }) {
  const campaigns = useQuery({ ...q.campaigns, enabled: v.kind === "campaign" });
  const lists = useQuery({ ...q.lists, enabled: v.kind === "list" });
  const accounts = useQuery({ ...q.accounts, enabled: v.kind === "account" });
  const providers = useQuery({ ...q.providers, enabled: v.kind === "provider" });
  const fields = useQuery({ ...q.fields, enabled: v.kind === "custom_field" });
  const options: { value: string; label: string }[] =
    v.kind === "campaign"
      ? (campaigns.data?.campaigns ?? []).map((c) => ({ value: c.id, label: c.name }))
      : v.kind === "list"
        ? (lists.data ?? []).map((l) => ({ value: l.id, label: l.name }))
        : v.kind === "account"
          ? (accounts.data ?? []).map((a) => ({ value: a.id, label: a.email }))
          : v.kind === "provider"
            ? [
                { value: "default_draft", label: "Default draft" },
                { value: "default_slot", label: "Default slot" },
                ...(providers.data ?? []).map((p) => ({ value: p.name, label: p.name })),
              ]
            : v.kind === "custom_field"
              ? (fields.data ?? []).map((f) => ({ value: f.key, label: f.label || f.key }))
              : [];
  const text = typeof v.value === "string" ? v.value : "";
  if (ID_KINDS.includes(v.kind)) {
    return (
      <select
        aria-label={v.label || v.key}
        value={text}
        onChange={(e) => onChange(e.target.value || null)}
        className={CONTROL}
      >
        <option value="">Choose…</option>
        {text && !options.some((o) => o.value === text) && <option value={text}>{text}</option>}
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
    );
  }
  if (v.kind === "number")
    return (
      <input
        aria-label={v.label || v.key}
        type="number"
        value={typeof v.value === "number" ? v.value : ""}
        onChange={(e) => onChange(e.target.value === "" ? null : Number(e.target.value))}
        className={CONTROL}
      />
    );
  if (v.kind === "boolean")
    return <Check2 label={v.label || v.key} checked={v.value === true} onChange={onChange} />;
  if (v.kind === "json")
    return (
      <JsonField
        label={v.label || v.key}
        value={v.value ?? undefined}
        onChange={(x) => onChange(x ?? null)}
        rows={3}
      />
    );
  return (
    <input
      aria-label={v.label || v.key}
      value={text}
      onChange={(e) => onChange(e.target.value || null)}
      className={CONTROL}
    />
  );
}

/** Test runs ask for a required variable that has no value. */
export function AskVariables({
  open,
  onClose,
  variables,
  onRun,
}: {
  open: boolean;
  onClose: () => void;
  variables: WfVariable[];
  onRun: (values: Record<string, unknown>) => void;
}) {
  const [values, setValues] = useState<Record<string, unknown>>({});
  const missing = variables.filter(
    (v) => values[v.key] === undefined || values[v.key] === null || values[v.key] === "",
  );
  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Values for this test"
      size="md"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button disabled={missing.length > 0} onClick={() => onRun(values)}>
            Run test
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        <p className="text-sm text-muted">
          These variables are required and have no value yet. The values are used for this test
          only. Set them for good in Workflow settings.
        </p>
        {variables.map((v) => (
          <div key={v.key} className="flex flex-col gap-1 text-sm">
            <span className="font-medium">{v.label || v.key}</span>
            <VarInput
              v={{ ...v, value: values[v.key] }}
              onChange={(val) => setValues({ ...values, [v.key]: val })}
            />
            {v.description && <span className="text-xs text-muted">{v.description}</span>}
          </div>
        ))}
      </div>
    </Modal>
  );
}

// ---------- workflow settings and variables ----------

const KINDS: VarKind[] = [
  "text",
  "number",
  "boolean",
  "campaign",
  "list",
  "sequence",
  "account",
  "provider",
  "custom_field",
  "json",
];

export function WorkflowSettings({
  open,
  onClose,
  def,
  onSave,
}: {
  open: boolean;
  onClose: () => void;
  def: WfDef;
  onSave: (patch: Pick<WfDef, "description" | "settings" | "variables">) => void;
}) {
  const workflowId = useEditor().workflowId;
  const [description, setDescription] = useState(def.description ?? "");
  const [settings, setSettings] = useState(def.settings ?? {});
  const [vars, setVars] = useState<WfVariable[]>(def.variables ?? []);
  const client = useQueryClient();
  useEffect(() => {
    if (open) {
      setDescription(def.description ?? "");
      setSettings(def.settings ?? {});
      setVars(def.variables ?? []);
    }
  }, [open]);
  const zones = useMemo(() => timezones(), []);
  const keys = vars.map((v) => v.key);
  const badKey = (v: WfVariable, i: number) => !VALID_KEY.test(v.key) || keys.indexOf(v.key) !== i;
  const bad = vars.some(badKey);
  const num = (k: string, min: number, max: number, label: string, help?: string) => (
    <label className="flex flex-col gap-1 text-sm">
      <span className="font-medium">{label}</span>
      <input
        type="number"
        min={min}
        max={max}
        value={typeof settings[k] === "number" ? (settings[k] as number) : ""}
        placeholder="default"
        onChange={(e) =>
          setSettings({
            ...settings,
            [k]: e.target.value === "" ? undefined : Number(e.target.value),
          })
        }
        className={CONTROL}
      />
      {help && <span className="text-xs text-muted">{help}</span>}
    </label>
  );
  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Workflow settings"
      size="lg"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button
            disabled={bad}
            onClick={() => {
              const clean = Object.fromEntries(
                Object.entries(settings).filter(([, v]) => v !== undefined && v !== ""),
              );
              onSave({ description: description.trim(), settings: clean, variables: vars });
              void client.invalidateQueries({ queryKey: ["automations", "list"] });
              onClose();
            }}
          >
            Save
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-5">
        <label className="flex flex-col gap-1 text-sm">
          <span className="font-medium">Description</span>
          <textarea
            value={description}
            maxLength={2000}
            rows={2}
            onChange={(e) => setDescription(e.target.value)}
            className="w-full rounded-(--radius-control) border border-border bg-(--input-bg) px-3 py-2 text-sm"
          />
        </label>
        <div className="grid gap-3 sm:grid-cols-2">
          <label className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Time zone</span>
            <select
              value={settings.timezone ?? ""}
              onChange={(e) => setSettings({ ...settings, timezone: e.target.value || undefined })}
              className={CONTROL}
            >
              <option value="">Sending default</option>
              {zones.map((z) => (
                <option key={z}>{z}</option>
              ))}
            </select>
            <span className="text-xs text-muted">For schedule and cron triggers.</span>
          </label>
          <label className="flex flex-col gap-1 text-sm">
            <span className="font-medium">When a node fails</span>
            <select
              value={settings.on_error ?? ""}
              onChange={(e) =>
                setSettings({ ...settings, on_error: (e.target.value || undefined) as never })
              }
              className={CONTROL}
            >
              <option value="">Stop the run (default)</option>
              <option value="continue">Continue with an error field</option>
            </select>
          </label>
          {num(
            "max_concurrent_runs",
            1,
            50,
            "Runs at the same time",
            "Default 5. More wait in the queue.",
          )}
          {num(
            "max_runs_per_hour",
            1,
            100000,
            "Runs per hour",
            "Default 1000. A guard against loops.",
          )}
          {num(
            "run_timeout_secs",
            60,
            86400,
            "Run time limit (seconds)",
            "Default 3600. Time in a long Wait does not count.",
          )}
          <label className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Keep data of production runs</span>
            <select
              value={settings.save_data ?? ""}
              onChange={(e) =>
                setSettings({ ...settings, save_data: (e.target.value || undefined) as never })
              }
              className={CONTROL}
            >
              <option value="">All (default)</option>
              <option value="errors_only">Only failed runs</option>
              <option value="none">None</option>
            </select>
          </label>
          <div className="flex flex-col gap-1 text-sm sm:col-span-2">
            <span className="font-medium">Error workflow</span>
            <WorkflowSelect
              label="Error workflow"
              value={
                typeof settings.error_workflow_id === "string" ? settings.error_workflow_id : ""
              }
              onChange={(v) => setSettings({ ...settings, error_workflow_id: v })}
              excludeId={workflowId}
              emptyLabel="None"
            />
            <span className="text-xs text-muted">
              When a production run of this workflow ends in an error, the published version of this
              workflow runs with the error details as its trigger item. It need not be active.
            </span>
          </div>
        </div>
        <section aria-label="Variables">
          <h3 className="mb-1 text-sm font-semibold">Variables</h3>
          <p className="mb-2 text-xs text-muted">
            Values that differ per install, such as a campaign. Read them as {"${{ vars.key }}"}.
          </p>
          <div className="flex flex-col gap-2">
            {vars.map((v, i) => (
              <div
                key={i}
                className="grid gap-2 rounded-(--radius-control) border border-border p-2 sm:grid-cols-[1fr_1fr_1fr_auto]"
              >
                <input
                  aria-label="Variable key"
                  value={v.key}
                  placeholder="key"
                  onChange={(e) =>
                    setVars(vars.map((x, j) => (j === i ? { ...x, key: e.target.value } : x)))
                  }
                  className={`${CONTROL} font-mono text-[13px] ${badKey(v, i) ? "border-danger" : ""}`}
                />
                <input
                  aria-label="Variable label"
                  value={v.label ?? ""}
                  placeholder="label"
                  onChange={(e) =>
                    setVars(vars.map((x, j) => (j === i ? { ...x, label: e.target.value } : x)))
                  }
                  className={CONTROL}
                />
                <select
                  aria-label="Variable kind"
                  value={v.kind}
                  onChange={(e) =>
                    setVars(
                      vars.map((x, j) =>
                        j === i ? { ...x, kind: e.target.value as VarKind, value: null } : x,
                      ),
                    )
                  }
                  className={CONTROL}
                >
                  {KINDS.map((k) => (
                    <option key={k}>{k}</option>
                  ))}
                </select>
                <button
                  type="button"
                  aria-label="Remove variable"
                  onClick={() => setVars(vars.filter((_, j) => j !== i))}
                  className="flex size-9 items-center justify-center rounded-(--radius-control) text-muted hover:bg-surface-2 hover:text-text"
                >
                  <Trash2 size={15} />
                </button>
                <div className="sm:col-span-3">
                  <VarInput
                    v={v}
                    onChange={(val) =>
                      setVars(vars.map((x, j) => (j === i ? { ...x, value: val } : x)))
                    }
                  />
                </div>
                <label className="flex items-center gap-2 text-xs text-muted sm:col-span-1">
                  <input
                    type="checkbox"
                    checked={!!v.required}
                    onChange={(e) =>
                      setVars(
                        vars.map((x, j) => (j === i ? { ...x, required: e.target.checked } : x)),
                      )
                    }
                    className="accent-(--accent)"
                  />{" "}
                  Required
                </label>
                {badKey(v, i) && (
                  <p className="text-xs text-danger sm:col-span-4">
                    Use a-z, 0-9 and _, start with a letter, and keep each key different.
                  </p>
                )}
              </div>
            ))}
            <button
              type="button"
              onClick={() =>
                setVars([
                  ...vars,
                  { key: "", label: "", kind: "text", value: null, required: false },
                ])
              }
              className="inline-flex h-8 w-fit items-center gap-1.5 rounded-(--radius-control) border border-dashed border-border px-2.5 text-xs text-muted hover:bg-surface-2 hover:text-text"
            >
              <Plus size={14} aria-hidden="true" /> Add a variable
            </button>
          </div>
        </section>
      </div>
    </Modal>
  );
}
