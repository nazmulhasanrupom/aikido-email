import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ArrowLeft, Copy, RotateCcw, Square, X } from "lucide-react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { Button, EmptyState, ErrorNote, Pill, Skeleton } from "@/components/ui";
import { fmtAgo, fmtDateTime } from "@/lib/format";
import { cn } from "@/lib/cn";
import { aq, call } from "./api";
import { FINISHED, type NodeStatus, type RunDetail, type RunRow, type RunStatus } from "./types";

type Tone = "neutral" | "success" | "warning" | "danger" | "info";

export const RUN_TONE: Record<RunStatus, Tone> = {
  queued: "neutral",
  running: "info",
  waiting: "warning",
  success: "success",
  error: "danger",
  cancelled: "neutral",
};

export const NODE_TONE: Record<NodeStatus, Tone> = {
  running: "info",
  success: "success",
  error: "danger",
  skipped: "neutral",
  dry: "info",
  waiting: "warning",
};

export function RunPill({ status }: { status: RunStatus }) {
  return (
    <Pill tone={RUN_TONE[status]} dot>
      {status}
    </Pill>
  );
}

export function duration(ms: number | null | undefined): string {
  if (ms === null || ms === undefined) return "";
  if (ms < 1000) return `${ms} ms`;
  const s = ms / 1000;
  if (s < 60) return `${s.toFixed(1)} s`;
  return `${Math.floor(s / 60)} min ${Math.round(s % 60)} s`;
}

const TH = "px-3 py-2 text-left text-[11px] font-semibold uppercase tracking-wider text-muted";

/** The Executions tab: runs of this workflow, newest first. Refreshes every 5 seconds. */
export function RunsTable({
  workflowId,
  mode,
  status,
  onMode,
  onStatus,
  onOpen,
}: {
  workflowId: string;
  mode: string;
  status: string;
  onMode: (v: string) => void;
  onStatus: (v: string) => void;
  onOpen: (id: string) => void;
}) {
  const runs = useQuery(aq.runs(workflowId, { mode: mode || undefined, status: status || undefined } as never));
  const rows = runs.data?.runs ?? [];
  return (
    <div className="mx-auto flex h-full w-full max-w-6xl flex-col gap-3 overflow-auto p-6">
      <div className="flex flex-wrap items-center gap-2">
        <select aria-label="Mode" value={mode} onChange={(e) => onMode(e.target.value)} className="h-9 rounded-(--radius-control) border border-border bg-surface px-3 text-sm">
          <option value="">Test and production</option>
          <option value="test">Test</option>
          <option value="production">Production</option>
        </select>
        <select aria-label="Status" value={status} onChange={(e) => onStatus(e.target.value)} className="h-9 rounded-(--radius-control) border border-border bg-surface px-3 text-sm">
          <option value="">All statuses</option>
          {(["queued", "running", "waiting", "success", "error", "cancelled"] as RunStatus[]).map((s) => (
            <option key={s}>{s}</option>
          ))}
        </select>
        <span className="text-xs text-muted">Updates every 5 seconds.</span>
      </div>
      {runs.isPending && <Skeleton rows={4} />}
      {runs.isError && <ErrorNote error={runs.error} onRetry={() => runs.refetch()} />}
      {runs.data && rows.length === 0 && <EmptyState title="No runs yet" text="Press Test workflow in the editor, or turn the workflow on." />}
      {rows.length > 0 && (
        <div className="overflow-x-auto rounded-(--radius-card) border border-border bg-surface">
          <table className="w-full border-collapse text-sm">
            <thead>
              <tr className="border-b border-border">
                {["Started", "Mode", "Status", "Version", "Duration", "Trigger", "Error"].map((h) => (
                  <th key={h} scope="col" className={TH}>
                    {h}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => (
                <Row key={r.id} r={r} onOpen={() => onOpen(r.id)} />
              ))}
            </tbody>
          </table>
        </div>
      )}
    </div>
  );
}

function Row({ r, onOpen }: { r: RunRow; onOpen: () => void }) {
  return (
    <tr className="cursor-pointer border-t border-border first:border-t-0 hover:bg-surface-2" onClick={onOpen}>
      <td className="px-3 py-2.5">
        <button type="button" onClick={onOpen} className="text-left" title={fmtDateTime(r.queued_at)}>
          {fmtAgo(r.started_at ?? r.queued_at)}
        </button>
      </td>
      <td className="px-3 py-2.5">
        <Pill tone={r.mode === "test" ? "neutral" : "accent"}>{r.mode}</Pill>
      </td>
      <td className="px-3 py-2.5">
        <RunPill status={r.status} />
      </td>
      <td className="px-3 py-2.5 text-muted">{r.version ? `v${r.version}` : "draft"}</td>
      <td className="px-3 py-2.5 text-muted">{duration(r.duration_ms)}</td>
      <td className="px-3 py-2.5 font-mono text-xs text-muted">{r.trigger_node_id}</td>
      <td className="px-3 py-2.5 font-mono text-xs text-danger">{r.error_code ?? ""}</td>
    </tr>
  );
}

/** The strip above a run that is open on the canvas. */
export function RunStrip({
  run,
  onBack,
  onCopyToEditor,
  onRetried,
}: {
  run: RunDetail;
  onBack: () => void;
  onCopyToEditor: () => void;
  onRetried: (runId: string) => void;
}) {
  const client = useQueryClient();
  const refresh = () => {
    void client.invalidateQueries({ queryKey: ["automations", "runs"] });
    void client.invalidateQueries({ queryKey: ["automations", "run", run.id] });
  };
  const retry = useMutation({
    mutationFn: () => call.retry(run.id),
    onSuccess: (r) => {
      toast.success("A new run was queued.");
      refresh();
      onRetried(r.run_id);
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const cancel = useMutation({
    mutationFn: () => call.cancel(run.id),
    onSuccess: () => {
      toast.success("Cancel requested.");
      refresh();
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const done = FINISHED.includes(run.status);
  return (
    <div className="flex flex-wrap items-center gap-3 border-b border-border bg-surface px-4 py-2">
      <Button variant="ghost" className="h-8 px-2" onClick={onBack}>
        <ArrowLeft size={15} aria-hidden="true" /> Runs
      </Button>
      <RunPill status={run.status} />
      <Pill tone={run.mode === "test" ? "neutral" : "accent"}>{run.mode}</Pill>
      <span className="text-sm text-muted">
        {run.version ? `v${run.version}` : "draft"} · {fmtDateTime(run.started_at ?? run.queued_at)} · {duration(run.duration_ms)}
      </span>
      {run.error && (
        <span className="min-w-0 truncate text-sm text-danger" title={run.error.message}>
          {run.error.code}: {run.error.message}
        </span>
      )}
      <span className="ml-auto flex gap-2">
        {run.mode === "production" && run.status === "error" && (
          <Button variant="secondary" className="h-8" busy={retry.isPending} onClick={() => retry.mutate()}>
            <RotateCcw size={14} aria-hidden="true" /> Retry
          </Button>
        )}
        {!done && (
          <Button variant="secondary" className="h-8" busy={cancel.isPending} onClick={() => cancel.mutate()}>
            <Square size={14} aria-hidden="true" /> Cancel
          </Button>
        )}
        <Button variant="secondary" className="h-8" onClick={onCopyToEditor} title="Use this run's trigger item as the pin of the trigger, so you can test the same case">
          <Copy size={14} aria-hidden="true" /> Copy to editor
        </Button>
      </span>
    </div>
  );
}

/** The bar under the canvas after a test (map 17.7). */
export function TestBar({
  run,
  live,
  error,
  nodeName,
  onOpen,
  onCancel,
  onClose,
  onSelectNode,
}: {
  run: RunDetail | null;
  live: boolean;
  error: string | null;
  nodeName: (id: string) => string;
  onOpen: () => void;
  onCancel: () => void;
  onClose: () => void;
  onSelectNode: (id: string) => void;
}) {
  const nodes = run?.nodes ?? [];
  const latest = new Map<string, (typeof nodes)[number]>();
  for (const n of nodes) {
    const old = latest.get(n.node_id);
    if (!old || n.seq > old.seq) latest.set(n.node_id, n);
  }
  const failed = [...latest.values()].find((n) => n.status === "error");
  return (
    <div role="status" className="flex flex-wrap items-center gap-x-4 gap-y-1 border-t border-border bg-surface px-4 py-2 text-sm">
      {run ? <RunPill status={run.status} /> : <Pill tone="info" dot>starting</Pill>}
      {run && <span className="text-muted">{duration(run.duration_ms ?? (run.started_at ? Date.now() - new Date(run.started_at).getTime() : null))}</span>}
      <ul className="flex min-w-0 flex-1 flex-wrap gap-1.5" aria-label="Nodes">
        {[...latest.values()].map((n) => (
          <li key={n.node_id}>
            <button
              type="button"
              onClick={() => onSelectNode(n.node_id)}
              className={cn(
                "rounded-full border px-2 py-0.5 text-xs",
                n.status === "error" ? "border-danger/50 text-danger" : n.status === "running" ? "border-info/50 text-info" : "border-border text-muted",
              )}
            >
              {nodeName(n.node_id)}
              {n.status !== "running" && n.status !== "skipped" ? ` · ${Object.values(n.output_count ?? {}).reduce((a, b) => a + b, 0)}` : ""}
            </button>
          </li>
        ))}
      </ul>
      {failed?.error && (
        <button type="button" onClick={() => onSelectNode(failed.node_id)} className="max-w-md truncate text-danger underline-offset-2 hover:underline" title={failed.error.message}>
          {failed.error.code}: {failed.error.message}
        </button>
      )}
      {run?.error && !failed?.error && <span className="max-w-md truncate text-danger">{run.error.code}: {run.error.message}</span>}
      {error && <span className="text-danger">{error}</span>}
      <span className="flex gap-1.5">
        {live && (
          <Button variant="secondary" className="h-7" onClick={onCancel}>
            Cancel
          </Button>
        )}
        {run && (
          <Button variant="secondary" className="h-7" onClick={onOpen}>
            Open run
          </Button>
        )}
        <button type="button" aria-label="Close the test bar" onClick={onClose} className="flex size-7 items-center justify-center rounded-(--radius-control) text-muted hover:bg-surface-2">
          <X size={15} />
        </button>
      </span>
    </div>
  );
}
