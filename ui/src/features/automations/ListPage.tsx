import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import {
  Download,
  KeyRound,
  MoreHorizontal,
  Plus,
  Search,
  Settings,
  Sparkles,
  Upload,
  Workflow,
} from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { MenuContent, MenuItem, MenuRoot, MenuTrigger } from "@/components/shell/menu";
import {
  Button,
  ConfirmDialog,
  EmptyState,
  ErrorNote,
  Notice,
  Page,
  PageHeader,
  Pill,
  Skeleton,
  useDebounced,
} from "@/components/ui";
import { useUser } from "@/features/settings/useUser";
import { fmtAgo } from "@/lib/format";
import { aq, call } from "./api";
import { AskAiCreate } from "./Copilot";
import { ActiveDialog } from "./Dialogs";
import { RunPill } from "./Executions";
import { ImportDialog, LlmGuideButton, useDownloader } from "./Transfer";
import type { WorkflowRow } from "./types";

const HEAD = "px-4 py-3 text-left text-[11px] font-semibold uppercase tracking-wider text-muted";

export function ListPage({ ws }: { ws: string }) {
  const navigate = useNavigate();
  const client = useQueryClient();
  const { isAdmin } = useUser();
  const [text, setText] = useState("");
  const [archived, setArchived] = useState(false);
  const term = useDebounced(text.trim(), 250);
  const list = useQuery(aq.workflows({ ...(term ? { q: term } : {}), archived }));
  const status = useQuery(aq.status);
  const [importing, setImporting] = useState(false);
  const downloader = useDownloader();
  const [toggle, setToggle] = useState<{ row: WorkflowRow; mode: "on" | "off" } | null>(null);
  const [archive, setArchive] = useState<WorkflowRow | null>(null);
  const rows = list.data?.workflows ?? [];
  const refresh = () => client.invalidateQueries({ queryKey: ["automations", "list"] });
  const resume = useMutation({
    mutationFn: () => call.resume(),
    onSuccess: () => {
      toast.success("The engine runs again.");
      void client.invalidateQueries({ queryKey: ["automations", "status"] });
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const open = (id: string) => navigate({ to: "/w/$ws/automations/$id", params: { ws, id } });

  const download = (row: WorkflowRow) =>
    downloader.start({
      id: row.id,
      hasPins: async () => Object.keys((await call.get(row.id)).draft.pins ?? {}).length > 0,
    });

  const actions = (
    <>
      <LlmGuideButton />
      <Button variant="secondary" onClick={() => setImporting(true)}>
        <Upload size={15} aria-hidden="true" /> Import
      </Button>
      <Link to="/w/$ws/automations/new" params={{ ws }}>
        <Button>
          <Plus size={16} aria-hidden="true" /> New workflow
        </Button>
      </Link>
    </>
  );

  return (
    <Page wide>
      <PageHeader
        title="Automations"
        sub="Node workflows for work that needs outside steps."
        actions={actions}
      />
      {status.data?.paused && (
        <div className="mb-4 flex flex-wrap items-center justify-between gap-3">
          <Notice tone="warning">
            The engine is paused. Production runs wait. Test runs still run.
          </Notice>
          {isAdmin && (
            <Button variant="secondary" busy={resume.isPending} onClick={() => resume.mutate()}>
              Resume the engine
            </Button>
          )}
        </div>
      )}
      {(status.data?.trigger_problems ?? []).map((p) => (
        <div key={`${p.workflow_id}${p.node_id}`} className="mb-2">
          <Notice tone="danger">
            <Link
              to="/w/$ws/automations/$id"
              params={{ ws, id: p.workflow_id }}
              className="font-medium underline"
            >
              {p.workflow_name}
            </Link>
            : the trigger {p.node_id} has a problem
            {p.last_error
              ? `: ${p.last_error}`
              : p.rate_limited_at
                ? ": it hit the runs-per-hour limit."
                : "."}
          </Notice>
        </div>
      ))}
      <section aria-label="Ask AI" className="mb-4 max-w-2xl">
        <p className="mb-1 flex items-center gap-1.5 text-sm font-medium">
          <Sparkles size={14} aria-hidden="true" className="text-ai" /> Ask AI to build a workflow
        </p>
        <AskAiCreate ws={ws} />
      </section>
      <div className="mb-3 flex flex-wrap items-center gap-3">
        <label className="relative inline-flex items-center">
          <span className="sr-only">Search</span>
          <Search
            size={15}
            aria-hidden="true"
            className="pointer-events-none absolute left-3 text-muted"
          />
          <input
            value={text}
            onChange={(e) => setText(e.target.value)}
            placeholder="Search"
            className="h-9 w-64 rounded-(--radius-control) border border-border bg-surface pr-3 pl-9 text-sm"
          />
        </label>
        <label className="flex items-center gap-2 text-sm text-muted">
          <input
            type="checkbox"
            checked={archived}
            onChange={(e) => setArchived(e.target.checked)}
            className="accent-(--accent)"
          />{" "}
          Show archived
        </label>
        <span className="ml-auto flex gap-1">
          {isAdmin && (
            <>
              <Link
                to="/w/$ws/automations/secrets"
                params={{ ws }}
                className="inline-flex h-9 items-center gap-1.5 rounded-(--radius-control) px-3 text-sm text-muted hover:bg-surface-2 hover:text-text"
              >
                <KeyRound size={15} aria-hidden="true" /> Secrets
              </Link>
              <Link
                to="/w/$ws/automations/settings"
                params={{ ws }}
                className="inline-flex h-9 items-center gap-1.5 rounded-(--radius-control) px-3 text-sm text-muted hover:bg-surface-2 hover:text-text"
              >
                <Settings size={15} aria-hidden="true" /> Settings
              </Link>
            </>
          )}
        </span>
      </div>
      {list.isPending && <Skeleton />}
      {list.isError && <ErrorNote error={list.error} onRetry={() => list.refetch()} />}
      {list.data && rows.length === 0 && !term && !archived && (
        <EmptyState
          icon={Workflow}
          title="No automations yet"
          text="Tip: build one by dragging nodes, or start from a file. Any AI can write a workflow file for you once it has the LLM guide."
          action={
            <div className="flex flex-wrap justify-center gap-2">
              <Link to="/w/$ws/automations/new" params={{ ws }}>
                <Button>Blank</Button>
              </Link>
              <Button variant="secondary" onClick={() => setImporting(true)}>
                Import a file
              </Button>
            </div>
          }
        />
      )}
      {list.data && rows.length === 0 && (term || archived) && (
        <p className="py-8 text-center text-muted">No workflow matches.</p>
      )}
      {rows.length > 0 && (
        <div className="overflow-x-auto rounded-(--radius-card) border border-border bg-surface">
          <table className="w-full border-collapse text-sm">
            <thead>
              <tr className="border-b border-border">
                {["Name", "Status", "Version", "Last run", "24 hours", "Tags", ""].map((h, i) => (
                  <th key={i} scope="col" className={HEAD}>
                    {h}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {rows.map((r) => (
                <tr
                  key={r.id}
                  className="border-t border-border first:border-t-0 hover:bg-surface-2"
                >
                  <td className="px-4 py-3">
                    <Link
                      to="/w/$ws/automations/$id"
                      params={{ ws, id: r.id }}
                      className="font-medium hover:underline"
                    >
                      {r.name}
                    </Link>
                    {r.description && (
                      <p className="max-w-md truncate text-xs text-muted">{r.description}</p>
                    )}
                  </td>
                  <td className="px-4 py-3">
                    <label className="inline-flex items-center gap-2">
                      <input
                        type="checkbox"
                        role="switch"
                        aria-checked={r.active}
                        aria-label={`Active: ${r.name}`}
                        checked={r.active}
                        disabled={!!r.archived_at}
                        onChange={() => {
                          if (r.active) setToggle({ row: r, mode: "off" });
                          else if (r.published_version === null) {
                            toast("Publish a version first. Opening the editor.");
                            void open(r.id);
                          } else setToggle({ row: r, mode: "on" });
                        }}
                        className="accent-(--accent)"
                      />
                      <Pill tone={r.active ? "success" : "neutral"} dot>
                        {r.active ? "Active" : "Inactive"}
                      </Pill>
                    </label>
                  </td>
                  <td className="px-4 py-3 text-muted">
                    {r.published_version ? `v${r.published_version}` : "Draft"}
                    {r.published_version !== null && r.draft_changed ? (
                      <span className="ml-1 text-xs text-warning">changed</span>
                    ) : null}
                    {!r.draft_valid && <span className="ml-1 text-xs text-danger">errors</span>}
                  </td>
                  <td className="px-4 py-3">
                    {r.last_run ? (
                      <span className="inline-flex items-center gap-2">
                        <RunPill status={r.last_run.status as never} />
                        <span className="text-xs text-muted">{fmtAgo(r.last_run.at)}</span>
                      </span>
                    ) : (
                      <span className="text-muted">Never</span>
                    )}
                  </td>
                  <td className="px-4 py-3 text-muted">
                    {r.runs_24h.success} ok ·{" "}
                    <span className={r.runs_24h.error ? "text-danger" : ""}>
                      {r.runs_24h.error} failed
                    </span>
                  </td>
                  <td className="px-4 py-3">
                    <span className="flex flex-wrap gap-1">
                      {r.tags.map((t) => (
                        <Pill key={t}>{t}</Pill>
                      ))}
                    </span>
                  </td>
                  <td className="px-4 py-3 text-right">
                    <MenuRoot>
                      <MenuTrigger
                        aria-label={`More for ${r.name}`}
                        className="inline-flex size-8 items-center justify-center rounded-(--radius-control) text-muted hover:bg-surface-2 hover:text-text"
                      >
                        <MoreHorizontal size={16} />
                      </MenuTrigger>
                      <MenuContent align="end">
                        <MenuItem
                          onSelect={() =>
                            void call
                              .duplicate(r.id)
                              .then((w) => {
                                toast.success("Duplicated.");
                                void refresh();
                                void open(w.id);
                              })
                              .catch((e) => toast.error(messageOf(e)))
                          }
                        >
                          Duplicate
                        </MenuItem>
                        <MenuItem onSelect={() => void download(r)}>
                          <Download size={15} aria-hidden="true" /> Download
                        </MenuItem>
                        {r.archived_at ? (
                          <MenuItem
                            onSelect={() =>
                              void call
                                .patch(r.id, { archived: false })
                                .then(() => {
                                  toast.success("Brought back.");
                                  void refresh();
                                })
                                .catch((e) => toast.error(messageOf(e)))
                            }
                          >
                            Bring back
                          </MenuItem>
                        ) : (
                          <MenuItem onSelect={() => setArchive(r)}>Archive</MenuItem>
                        )}
                      </MenuContent>
                    </MenuRoot>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <ImportDialog open={importing} onClose={() => setImporting(false)} ws={ws} />
      {downloader.dialog}
      <ActiveDialog
        open={toggle !== null}
        mode={toggle?.mode ?? "on"}
        onClose={() => setToggle(null)}
        workflowId={toggle?.row.id ?? ""}
        published={toggle?.row.published_version ?? null}
        onDone={() => {
          void refresh();
          void client.invalidateQueries({ queryKey: ["automations", "status"] });
        }}
      />
      <ConfirmDialog
        open={archive !== null}
        onClose={() => setArchive(null)}
        title="Archive this workflow?"
        text={
          archive
            ? `"${archive.name}" is switched off and hidden. You can bring it back later.`
            : ""
        }
        confirmLabel="Archive"
        danger={false}
        onConfirm={async () => {
          if (!archive) return;
          await call.remove(archive.id);
          toast.success("Archived.");
          setArchive(null);
          void refresh();
        }}
      />
    </Page>
  );
}
