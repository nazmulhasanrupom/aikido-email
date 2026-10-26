import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import { BookOpen, Check, ChevronDown, ChevronRight, X } from "lucide-react";
import { type ReactNode, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { q } from "@/api/queries";
import { Button, Modal, Notice, Pill } from "@/components/ui";
import { cn } from "@/lib/cn";
import { aq, call } from "./api";
import { CONTROL } from "./ParamForm";
import {
  actionsFor,
  excerpt,
  fileNameFrom,
  type ImportCheckReply,
  type ImportReport,
  type ImportSource,
  issueText,
  jsonPlace,
  keepReport,
  type ReportAction,
  reportHasNews,
  saveBlob,
  setNodeProvider,
  setVariableValue,
} from "./transfer";
import type { WfDef } from "./types";

// ---------- download ----------

interface DownloadTarget {
  id: string;
  version?: number;
  /** Whether the file would have pins. Asked only then. */
  hasPins: () => Promise<boolean> | boolean;
}

/** Download a workflow file through the export route. Asks about pins when there are some. */
export function useDownloader() {
  const [ask, setAsk] = useState<DownloadTarget | null>(null);
  const [busy, setBusy] = useState(false);

  async function run(t: DownloadTarget, includePins: boolean) {
    setBusy(true);
    try {
      const r = await call.exportFile(t.id, {
        ...(t.version !== undefined ? { version: t.version } : {}),
        includePins,
      });
      saveBlob(fileNameFrom(r.disposition, "workflow"), r.text, "application/json");
    } catch (e) {
      toast.error(messageOf(e));
    } finally {
      setBusy(false);
      setAsk(null);
    }
  }

  async function start(t: DownloadTarget) {
    let pins = false;
    try {
      pins = await t.hasPins();
    } catch (e) {
      toast.error(messageOf(e));
      return;
    }
    if (pins) setAsk(t);
    else await run(t, false);
  }

  const dialog = (
    <Modal
      open={ask !== null}
      onClose={() => setAsk(null)}
      title="Include the saved test data?"
      size="sm"
      footer={
        <>
          <Button variant="secondary" onClick={() => setAsk(null)}>
            Cancel
          </Button>
          <Button variant="secondary" busy={busy} onClick={() => ask && void run(ask, false)}>
            Leave them out
          </Button>
          <Button busy={busy} onClick={() => ask && void run(ask, true)}>
            Include pins
          </Button>
        </>
      }
    >
      <p className="text-sm">Pins can hold lead data. Include them?</p>
    </Modal>
  );
  return { start, dialog };
}

// ---------- import ----------

type Tab = "file" | "paste" | "url";

/** Import from a file, pasted JSON or a URL. On success the editor opens with the report. */
export function ImportDialog({
  open,
  onClose,
  ws,
}: {
  open: boolean;
  onClose: () => void;
  ws: string;
}) {
  const navigate = useNavigate();
  const client = useQueryClient();
  const [tab, setTab] = useState<Tab>("file");
  const [pasted, setPasted] = useState("");
  const [url, setUrl] = useState("");
  const [name, setName] = useState("");
  const [fileText, setFileText] = useState<{ name: string; text: string } | null>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<{
    message: string;
    text?: string;
    line?: number;
    column?: number;
  } | null>(null);
  const picker = useRef<HTMLInputElement>(null);

  function source(): ImportSource | null {
    if (tab === "file") return fileText ? { text: fileText.text } : null;
    if (tab === "paste") return pasted.trim() ? { text: pasted } : null;
    return url.trim() ? { url: url.trim() } : null;
  }

  async function go() {
    const src = source();
    if (!src) return;
    setBusy(true);
    setProblem(null);
    try {
      const r = await call.importWorkflow(src, name.trim() ? { name: name.trim() } : {});
      keepReport(r.workflow.id, r.report);
      void client.invalidateQueries({ queryKey: ["automations", "list"] });
      toast.success("Imported as a draft. It is not active.");
      onClose();
      void navigate({ to: "/w/$ws/automations/$id", params: { ws, id: r.workflow.id } });
    } catch (e) {
      const place = jsonPlace(e);
      setProblem({
        message: messageOf(e),
        ...("text" in src && place
          ? { text: src.text, line: place.line, column: place.column }
          : {}),
      });
    } finally {
      setBusy(false);
    }
  }

  const tabs: [Tab, string][] = [
    ["file", "File"],
    ["paste", "Paste JSON"],
    ["url", "From a link"],
  ];
  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Import a workflow"
      size="lg"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button busy={busy} disabled={!source()} onClick={() => void go()}>
            Import
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        <div role="tablist" aria-label="Where from" className="flex gap-1">
          {tabs.map(([k, label]) => (
            <button
              key={k}
              type="button"
              role="tab"
              aria-selected={tab === k}
              onClick={() => {
                setTab(k);
                setProblem(null);
              }}
              className={cn(
                "rounded-(--radius-control) px-3 py-1 text-sm",
                tab === k
                  ? "bg-surface-2 font-medium"
                  : "text-muted hover:bg-surface-2 hover:text-text",
              )}
            >
              {label}
            </button>
          ))}
        </div>
        {tab === "file" && (
          <div className="flex items-center gap-3">
            <input
              ref={picker}
              type="file"
              accept=".json,application/json"
              className="sr-only"
              aria-label="Workflow file"
              onChange={async (e) => {
                const f = e.target.files?.[0];
                if (!f) return;
                setFileText({ name: f.name, text: await f.text() });
                setProblem(null);
              }}
            />
            <Button variant="secondary" onClick={() => picker.current?.click()}>
              Choose a file
            </Button>
            <span className="truncate text-sm text-muted">
              {fileText ? fileText.name : "A .aikido-workflow.json file, or any JSON an AI wrote."}
            </span>
          </div>
        )}
        {tab === "paste" && (
          <textarea
            aria-label="Workflow JSON"
            value={pasted}
            onChange={(e) => {
              setPasted(e.target.value);
              setProblem(null);
            }}
            rows={10}
            spellCheck={false}
            placeholder='{ "format": "aikido-workflow", ... }'
            className={cn(CONTROL, "h-auto py-2 font-mono text-xs")}
          />
        )}
        {tab === "url" && (
          <label className="flex flex-col gap-1 text-sm">
            Link to a JSON file
            <input
              value={url}
              onChange={(e) => {
                setUrl(e.target.value);
                setProblem(null);
              }}
              placeholder="https://github.com/you/flows/blob/main/lead.aikido-workflow.json"
              className={CONTROL}
            />
            <span className="text-xs text-muted">
              Public https links only, up to 2 MB. A GitHub page link works.
            </span>
          </label>
        )}
        <label className="flex flex-col gap-1 text-sm">
          Name (optional)
          <input
            value={name}
            onChange={(e) => setName(e.target.value)}
            maxLength={120}
            placeholder="Use the name in the file"
            className={CONTROL}
          />
        </label>
        {problem && <ProblemView problem={problem} />}
        <p className="text-xs text-muted">
          The workflow is saved as a new draft. It is never active or published until you do that.
        </p>
      </div>
    </Modal>
  );
}

function ProblemView({
  problem,
}: {
  problem: { message: string; text?: string; line?: number; column?: number };
}) {
  const ex =
    problem.text && problem.line
      ? excerpt(problem.text, { line: problem.line, column: problem.column ?? 1 })
      : null;
  return (
    <div
      role="alert"
      className="flex flex-col gap-2 rounded-(--radius-control) border border-danger/40 bg-danger/10 p-3 text-sm"
    >
      <p className="font-medium">{problem.message}</p>
      {problem.line && (
        <p className="text-xs text-muted">
          Line {problem.line}, column {problem.column}
        </p>
      )}
      {ex && (
        <pre className="overflow-x-auto rounded bg-surface-2 p-2 font-mono text-xs">
          {ex.line}
          {"\n"}
          {ex.caret}
        </pre>
      )}
    </div>
  );
}

// ---------- the import report ----------

function ReportList({
  title,
  items,
  tone,
}: {
  title: string;
  items: string[];
  tone?: "warning" | "danger";
}) {
  if (items.length === 0) return null;
  return (
    <div>
      <h3 className="mb-1 text-xs font-semibold uppercase tracking-wider text-muted">{title}</h3>
      <ul
        className={cn(
          "flex list-disc flex-col gap-0.5 pl-5 text-sm",
          tone === "danger" && "text-danger",
          tone === "warning" && "text-warning",
        )}
      >
        {items.map((t, i) => (
          <li key={i}>{t}</li>
        ))}
      </ul>
    </div>
  );
}

/**
 * The panel after an import (or after a pasted workflow): what was fixed, what to look at, and a button per thing that needs the person.
 * Picking a value changes the draft through `update`.
 */
export function ImportReportPanel({
  ws,
  report,
  def,
  update,
  onClose,
  onSelectNode,
}: {
  ws: string;
  report: ImportReport;
  def: WfDef;
  update: (fn: (d: WfDef) => WfDef) => void;
  onClose: () => void;
  onSelectNode?: (id: string) => void;
}) {
  const secrets = useQuery(aq.secrets);
  const names = useMemo(() => new Set((secrets.data ?? []).map((s) => s.name)), [secrets.data]);
  const actions = actionsFor(report, def, names);
  const open = actions.filter((a) => !a.done);
  const [shown, setShown] = useState(true);
  const v = report.validation;
  return (
    <section
      aria-label="Import report"
      className="border-b border-border bg-surface-2/50 px-4 py-3 text-sm"
    >
      <div className="flex items-center gap-2">
        <button
          type="button"
          onClick={() => setShown((s) => !s)}
          aria-expanded={shown}
          className="flex items-center gap-1 font-semibold"
        >
          {shown ? (
            <ChevronDown size={15} aria-hidden="true" />
          ) : (
            <ChevronRight size={15} aria-hidden="true" />
          )}{" "}
          Import report
        </button>
        {open.length > 0 ? (
          <Pill tone="warning">{open.length} to do</Pill>
        ) : (
          <Pill tone="success">Nothing left to do</Pill>
        )}
        {!v.ok && <Pill tone="danger">{v.errors.length} errors</Pill>}
        <button
          type="button"
          aria-label="Close the report"
          onClick={onClose}
          className="ml-auto flex size-7 items-center justify-center rounded-(--radius-control) text-muted hover:bg-surface-2 hover:text-text"
        >
          <X size={15} />
        </button>
      </div>
      {shown && (
        <div className="mt-3 flex max-h-72 flex-col gap-3 overflow-y-auto">
          {actions.length > 0 && (
            <div>
              <h3 className="mb-1 text-xs font-semibold uppercase tracking-wider text-muted">
                Needs you
              </h3>
              <ul className="flex flex-col gap-2">
                {actions.map((a) => (
                  <ActionRow key={a.id} ws={ws} a={a} update={update} onSelectNode={onSelectNode} />
                ))}
              </ul>
            </div>
          )}
          <ReportList title="Fixed for you" items={report.fixed} />
          <ReportList title="Warnings" items={report.warnings} tone="warning" />
          {v.errors.length > 0 && (
            <div>
              <h3 className="mb-1 text-xs font-semibold uppercase tracking-wider text-muted">
                Errors in the workflow
              </h3>
              <ul className="flex flex-col gap-0.5">
                {v.errors.map((i, k) => (
                  <li key={k} className="text-danger">
                    {i.node_id && onSelectNode ? (
                      <button
                        type="button"
                        className="underline"
                        onClick={() => onSelectNode(i.node_id as string)}
                      >
                        {i.node_id}
                      </button>
                    ) : null}
                    {i.node_id && onSelectNode ? `: ${i.message}` : issueText(i)}
                  </li>
                ))}
              </ul>
            </div>
          )}
          <ReportList title="Checks" items={v.warnings.map(issueText)} tone="warning" />
          {report.source && (report.source.exported_at || report.source.app_version) && (
            <p className="text-xs text-muted">
              From a file exported{" "}
              {report.source.exported_at
                ? new Date(report.source.exported_at).toLocaleString()
                : ""}
              {report.source.app_version ? ` by AIKIDO EMAIL ${report.source.app_version}` : ""}.
            </p>
          )}
          {!reportHasNews(report) && <p className="text-muted">The file imported cleanly.</p>}
        </div>
      )}
    </section>
  );
}

function ActionRow({
  ws,
  a,
  update,
  onSelectNode,
}: {
  ws: string;
  a: ReportAction;
  update: (fn: (d: WfDef) => WfDef) => void;
  onSelectNode: ((id: string) => void) | undefined;
}) {
  return (
    <li
      className={cn(
        "flex flex-col gap-2 rounded-(--radius-control) border border-border bg-surface p-2.5 sm:flex-row sm:items-center sm:justify-between",
        a.done && "opacity-60",
      )}
    >
      <div className="flex min-w-0 items-start gap-2">
        {a.done ? (
          <Check size={15} aria-hidden="true" className="mt-0.5 shrink-0 text-success" />
        ) : (
          <span className="mt-1.5 size-2 shrink-0 rounded-full bg-warning" aria-hidden="true" />
        )}
        <p className="min-w-0">{a.message}</p>
      </div>
      <div className="shrink-0">
        {a.kind === "pick" && <PickControl a={a} update={update} />}
        {a.kind === "id" && <IdControl a={a} update={update} />}
        {a.kind === "provider" && (
          <ProviderControl a={a} update={update} onSelectNode={onSelectNode} />
        )}
        {a.kind === "secret" && !a.done && (
          <Link
            to="/w/$ws/automations/secrets"
            params={{ ws }}
            className="inline-flex h-8 items-center rounded-(--radius-control) border border-border px-3 text-sm hover:bg-surface-2"
          >
            Add secret {a.key}
          </Link>
        )}
      </div>
    </li>
  );
}

function PickControl({
  a,
  update,
}: {
  a: ReportAction;
  update: (fn: (d: WfDef) => WfDef) => void;
}) {
  const kind = a.varKind ?? "";
  const campaigns = useQuery({ ...q.campaigns, enabled: kind === "campaign" });
  const lists = useQuery({ ...q.lists, enabled: kind === "list" });
  const accounts = useQuery({ ...q.accounts, enabled: kind === "account" });
  const providers = useQuery({ ...q.providers, enabled: kind === "provider" });
  const fields = useQuery({ ...q.fields, enabled: kind === "custom_field" });
  const options: { value: string; label: string }[] = [];
  if (kind === "campaign")
    for (const c of campaigns.data?.campaigns ?? []) options.push({ value: c.id, label: c.name });
  if (kind === "list")
    for (const l of lists.data ?? []) options.push({ value: l.id, label: l.name });
  if (kind === "account")
    for (const x of accounts.data ?? []) options.push({ value: x.id, label: x.email });
  if (kind === "provider")
    for (const p of providers.data ?? [])
      options.push({ value: p.id, label: `${p.name} (${p.model})` });
  if (kind === "custom_field")
    for (const f of fields.data ?? []) options.push({ value: f.key, label: f.label || f.key });
  const noun = kind.replace("_", " ");
  return (
    <select
      aria-label={`Pick the ${noun} for ${a.key}`}
      defaultValue=""
      onChange={(e) =>
        e.target.value && update((d) => setVariableValue(d, a.key, e.target.value, kind))
      }
      className={cn(CONTROL, "h-8 w-56")}
    >
      <option value="">{a.done ? `Change the ${noun}…` : `Pick the ${noun}…`}</option>
      {options.map((o) => (
        <option key={o.value} value={o.value}>
          {o.label}
        </option>
      ))}
    </select>
  );
}

function IdControl({ a, update }: { a: ReportAction; update: (fn: (d: WfDef) => WfDef) => void }) {
  const [text, setText] = useState("");
  return (
    <form
      className="flex gap-2"
      onSubmit={(e) => {
        e.preventDefault();
        if (text.trim()) update((d) => setVariableValue(d, a.key, text.trim(), a.varKind));
      }}
    >
      <input
        aria-label={`Id for ${a.key}`}
        value={text}
        onChange={(e) => setText(e.target.value)}
        placeholder="Paste the id"
        className={cn(CONTROL, "h-8 w-44")}
      />
      <Button type="submit" variant="secondary" className="h-8" disabled={!text.trim()}>
        Set
      </Button>
    </form>
  );
}

function ProviderControl({
  a,
  update,
  onSelectNode,
}: {
  a: ReportAction;
  update: (fn: (d: WfDef) => WfDef) => void;
  onSelectNode: ((id: string) => void) | undefined;
}) {
  const providers = useQuery(q.providers);
  return (
    <span className="flex items-center gap-2">
      {onSelectNode && (
        <button type="button" className="text-xs underline" onClick={() => onSelectNode(a.key)}>
          Show node
        </button>
      )}
      <select
        aria-label={`Provider for ${a.key}`}
        defaultValue=""
        onChange={(e) => e.target.value && update((d) => setNodeProvider(d, a.key, e.target.value))}
        className={cn(CONTROL, "h-8 w-52")}
      >
        <option value="">Pick a provider…</option>
        <option value="default_draft">Default draft (strong)</option>
        <option value="default_slot">Default slot (cheap)</option>
        {(providers.data ?? []).map((p) => (
          <option key={p.id} value={p.name}>
            {p.name} ({p.model})
          </option>
        ))}
      </select>
    </span>
  );
}

// ---------- paste a whole workflow onto the canvas ----------

export type PasteChoice = "replace" | "add" | "cancel";

export function PasteWorkflowDialog({
  checking,
  result,
  onChoose,
}: {
  checking: boolean;
  result: ImportCheckReply | null;
  onChoose: (c: PasteChoice) => void;
}) {
  const open = checking || result !== null;
  const nodes = result?.definition.nodes?.length ?? 0;
  const r = result?.report;
  return (
    <Modal
      open={open}
      onClose={() => onChoose("cancel")}
      title="Paste a workflow"
      size="md"
      footer={
        <>
          <Button variant="secondary" onClick={() => onChoose("cancel")}>
            Cancel
          </Button>
          <Button variant="secondary" disabled={!result} onClick={() => onChoose("add")}>
            Add the nodes
          </Button>
          <Button disabled={!result} onClick={() => onChoose("replace")}>
            Replace this workflow
          </Button>
        </>
      }
    >
      {checking && !result && <p className="text-sm text-muted">Checking the pasted workflow…</p>}
      {result && r && (
        <div className="flex flex-col gap-3 text-sm">
          <p>
            You pasted <strong>{result.definition.name || "a workflow"}</strong> with {nodes}{" "}
            {nodes === 1 ? "node" : "nodes"}. Replace the open workflow with it, or add its nodes
            next to the ones you have?
          </p>
          {r.fixed.length > 0 && <ReportList title="Fixed for you" items={r.fixed} />}
          {r.needs_you.length > 0 && (
            <ReportList
              title="Needs you afterwards"
              items={r.needs_you.map((n) => n.message)}
              tone="warning"
            />
          )}
          {r.validation.errors.length > 0 && (
            <Notice tone="warning">
              {r.validation.errors.length} errors in the pasted workflow. You can fix them in the
              editor.
            </Notice>
          )}
          <p className="text-xs text-muted">
            Nothing is changed until you choose. Undo (Ctrl+Z) takes it back.
          </p>
        </div>
      )}
    </Modal>
  );
}

// ---------- LLM guide ----------

export function LlmGuideButton(): ReactNode {
  const [open, setOpen] = useState(false);
  const [names, setNames] = useState(false);
  const [busy, setBusy] = useState(false);
  async function go() {
    setBusy(true);
    try {
      const r = await call.workprocess(names);
      saveBlob("workprocess.md", r.text, "text/markdown");
      setOpen(false);
    } catch (e) {
      toast.error(messageOf(e));
    } finally {
      setBusy(false);
    }
  }
  return (
    <>
      <Button variant="secondary" onClick={() => setOpen(true)}>
        <BookOpen size={15} aria-hidden="true" /> LLM guide
      </Button>
      <Modal
        open={open}
        onClose={() => setOpen(false)}
        title="LLM guide"
        size="md"
        footer={
          <>
            <Button variant="secondary" onClick={() => setOpen(false)}>
              Cancel
            </Button>
            <Button busy={busy} onClick={() => void go()}>
              Download workprocess.md
            </Button>
          </>
        }
      >
        <div className="flex flex-col gap-3 text-sm">
          <p>
            Give this file to any AI chat together with your request. It teaches the AI to write a
            workflow file that you can import here.
          </p>
          <label className="flex items-start gap-2">
            <input
              type="checkbox"
              checked={names}
              onChange={(e) => setNames(e.target.checked)}
              className="mt-1 accent-(--accent)"
            />
            <span>
              Include names of your campaigns, lists and fields?
              <span className="block text-xs text-muted">
                Off by default. When on, the file lists the names and ids of this install, so the AI
                can use them. Secret values are never in it.
              </span>
            </span>
          </label>
        </div>
      </Modal>
    </>
  );
}
