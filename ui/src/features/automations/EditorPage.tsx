import { useQuery, useQueryClient } from "@tanstack/react-query";
import { Link, useNavigate } from "@tanstack/react-router";
import {
  ArrowLeft,
  Download,
  FlaskConical,
  History,
  MoreHorizontal,
  Redo2,
  Rocket,
  Settings2,
  Undo2,
} from "lucide-react";
import { type ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { ApiProblem, messageOf } from "@/api/errors";
import {
  MenuContent,
  MenuItem,
  MenuRoot,
  MenuSeparator,
  MenuTrigger,
} from "@/components/shell/menu";
import { Button, ErrorNote, Modal, Pill, Skeleton } from "@/components/ui";
import { cn } from "@/lib/cn";
import { fmtDateTime } from "@/lib/format";
import { aq, call } from "./api";
import { Canvas } from "./Canvas";
import { EditorProvider, groupIssues } from "./ctx";
import { ActiveDialog, AskVariables, PublishDialog, WorkflowSettings } from "./Dialogs";
import {
  addNode,
  findNode,
  fragmentOf,
  isTrigger,
  outPorts,
  pasteFragment,
  pruneEdges,
  publishSignature,
  removeNodes,
  renameNode,
  upstream,
} from "./defs";
import { RunStrip, RunsTable, TestBar } from "./Executions";
import { needsLayout, withTidyPositions } from "./layout";
import { NodePanel } from "./NodePanel";
import { Palette } from "./Palette";
import {
  ImportReportPanel,
  type PasteChoice,
  PasteWorkflowDialog,
  useDownloader,
} from "./Transfer";
import type { Listening } from "./TriggerTools";
import { type TestInput, testBody } from "./testInput";
import {
  addWorkflowNodes,
  type ImportCheckReply,
  type ImportReport,
  renameNote,
  takeReport,
} from "./transfer";
import type { NodeSpec, Obj, Validation, WfDef, WfNode, WorkflowDetail } from "./types";
import { useDraft } from "./useDraft";
import { useLiveRun } from "./useRun";

interface Props {
  ws: string;
  id: string;
  tab: "editor" | "executions";
  runId: string | undefined;
}

export function EditorPage(p: Props) {
  const wf = useQuery({
    ...aq.workflow(p.id),
    staleTime: Number.POSITIVE_INFINITY,
    refetchOnWindowFocus: false,
    refetchOnReconnect: false,
  });
  const catalog = useQuery(aq.catalog);
  if (wf.isPending || catalog.isPending) {
    return (
      <div className="p-6">
        <Skeleton rows={4} />
      </div>
    );
  }
  if (wf.isError || catalog.isError) {
    return (
      <div className="p-6">
        <ErrorNote
          error={wf.error ?? catalog.error}
          onRetry={() => (wf.isError ? wf.refetch() : catalog.refetch())}
        />
        <Link
          to="/w/$ws/automations"
          params={{ ws: p.ws }}
          className="mt-3 inline-block text-sm underline"
        >
          Back to Automations
        </Link>
      </div>
    );
  }
  return <Editor key={p.id} {...p} wf={wf.data} specs={catalog.data} />;
}

interface Meta {
  active: boolean;
  published: WorkflowDetail["published"];
  archived: boolean;
}

function Editor({
  ws,
  id,
  tab,
  runId,
  wf,
  specs: specList,
}: Props & { wf: WorkflowDetail; specs: NodeSpec[] }) {
  const navigate = useNavigate();
  const client = useQueryClient();
  const draft = useDraft(wf);
  const live = useLiveRun();
  const [meta, setMeta] = useState<Meta>({
    active: wf.active,
    published: wf.published,
    archived: !!wf.archived_at,
  });
  const [selected, setSelected] = useState<string[]>([]);
  const [realWrites, setRealWrites] = useState(false);
  const [testInputs, setTestInputs] = useState<Record<string, TestInput>>({});
  const [listening, setListening] = useState<Listening | null>(null);
  const [showBar, setShowBar] = useState(false);
  const [busy, setBusy] = useState(false);
  const [dialog, setDialog] = useState<
    null | "publish" | "on" | "off" | "settings" | "versions" | "vars"
  >(null);
  const [askVars, setAskVars] = useState<{ body: TestRequest } | null>(null);
  const [saveTick, setSaveTick] = useState(0);
  const [mode, setMode] = useState("");
  const [status, setStatus] = useState("");
  const paletteInput = useRef<HTMLInputElement>(null);
  const downloader = useDownloader();
  const [report, setReport] = useState<ImportReport | null>(() => takeReport(id));
  const [pasting, setPasting] = useState<{ checking: boolean; result: ImportCheckReply | null }>({
    checking: false,
    result: null,
  });

  const specs = useMemo(() => new Map(specList.map((s) => [s.type, s])), [specList]);
  const { def, update } = draft;

  draft.onSaved((w) => {
    setSaveTick((t) => t + 1);
    setMeta((m) => ({ ...m, active: w.active, published: w.published, archived: !!w.archived_at }));
  });

  // a workflow with no positions (written by a tool or an LLM) is laid out once
  useEffect(() => {
    if (needsLayout(wf.draft)) update((d) => withTidyPositions(d), { record: false });
  }, []);

  // ---- a whole workflow pasted on the canvas ----
  async function pasteWorkflow(text: string) {
    setPasting({ checking: true, result: null });
    try {
      setPasting({ checking: true, result: await call.importCheck({ text }) });
    } catch (e) {
      setPasting({ checking: false, result: null });
      toast.error(messageOf(e));
    }
  }

  function choosePaste(choice: PasteChoice) {
    const result = pasting.result;
    setPasting({ checking: false, result: null });
    if (!result || choice === "cancel") return;
    const incoming = result.definition;
    if (choice === "replace") {
      update((d) => ({ ...incoming, name: d.name }));
      setSelected([]);
      toast.success("The pasted workflow replaced this one. Undo brings the old one back.");
    } else {
      let added: ReturnType<typeof addWorkflowNodes> | null = null;
      update((d) => {
        added = addWorkflowNodes(d, incoming);
        return added.def;
      });
      const a = added as ReturnType<typeof addWorkflowNodes> | null;
      if (a) setSelected(a.ids);
      toast.success(`Added ${a?.ids.length ?? 0} nodes. ${a ? renameNote(a.renamed) : ""}`.trim());
    }
    if (
      result.report.needs_you.length > 0 ||
      result.report.warnings.length > 0 ||
      !result.report.validation.ok
    )
      setReport(result.report);
  }

  // ---- the run on the canvas ----
  useEffect(() => {
    if (runId) void live.show(runId);
    else if (tab === "editor" && live.runId && !showBar) live.clear();
  }, [runId, tab]);

  const viewing = tab === "executions" && !!runId;
  const run = live.run;
  const version = run?.version ?? null;
  const runDef = useQuery({
    ...aq.version(id, version ?? 0),
    enabled: viewing && version !== null,
  });
  const viewDef: WfDef = viewing && version !== null && runDef.data ? runDef.data.definition : def;

  const newest = useQuery({
    queryKey: ["automations", "newest-test", id],
    queryFn: async () => {
      const list = await call.listRuns(id, "test");
      const first = list.runs[0];
      return first ? call.runDetail(first.id) : null;
    },
    staleTime: 30_000,
  });
  const dataRun = run ?? newest.data ?? null;
  useEffect(() => {
    // a test ended: its data is the newest
    if (!live.live && live.run && live.run.mode === "test")
      void client.invalidateQueries({ queryKey: ["automations", "newest-test", id] });
    if (!live.live && live.run)
      void client.invalidateQueries({ queryKey: ["automations", "runs", id] });
  }, [live.live, live.run?.id]);

  const publishedDef = useQuery({
    ...aq.version(id, meta.published?.version ?? 0),
    enabled: meta.published !== null,
  });
  const changed =
    meta.published !== null &&
    publishedDef.data !== undefined &&
    publishSignature(publishedDef.data.definition) !== publishSignature(def);

  const engine = useQuery(aq.status);
  const isAdmin = true;

  const issues = useMemo(
    () => groupIssues(draft.validation.errors, draft.validation.warnings),
    [draft.validation],
  );
  const readOnly = viewing || meta.archived;

  // ---- editing helpers ----
  const panelNode =
    !viewing || run
      ? selected.length === 1
        ? findNode(viewDef, selected[0])
        : undefined
      : undefined;
  const panelSpec = panelNode ? specs.get(panelNode.type) : undefined;

  function setParams(nodeId: string, params: Obj) {
    update(
      (d) => {
        const next = { ...d, nodes: d.nodes.map((n) => (n.id === nodeId ? { ...n, params } : n)) };
        const node = next.nodes.find((n) => n.id === nodeId);
        return pruneEdges(next, nodeId, node ? specs.get(node.type) : undefined);
      },
      { key: `p:${nodeId}` },
    );
  }

  function patch(nodeId: string, p: Partial<WfNode>) {
    update(
      (d) => {
        const next = {
          ...d,
          nodes: d.nodes.map((n) => {
            if (n.id !== nodeId) return n;
            const merged: Record<string, unknown> = { ...n, ...p };
            for (const k of Object.keys(merged)) if (merged[k] === undefined) delete merged[k];
            return merged as unknown as WfNode;
          }),
        };
        const node = next.nodes.find((n) => n.id === nodeId);
        return pruneEdges(next, nodeId, node ? specs.get(node.type) : undefined);
      },
      { key: `n:${nodeId}:${Object.keys(p).join(",")}` },
    );
  }

  function addFromPalette(spec: NodeSpec) {
    if (readOnly) return;
    const after = selected.length === 1 ? findNode(def, selected[0]) : undefined;
    const afterSpec = after ? specs.get(after.type) : undefined;
    const port = after ? (outPorts(after, afterSpec)[0] ?? null) : null;
    let at = { x: 80, y: 120 };
    if (after?.position) {
      const kids = def.edges.filter((e) => e.from === after.id).length;
      at = { x: after.position.x + 280, y: after.position.y + kids * 90 };
    } else if (def.nodes.length > 0) {
      const xs = def.nodes.map((n) => n.position?.x ?? 0);
      const right = def.nodes.find((n) => (n.position?.x ?? 0) === Math.max(...xs));
      at = { x: Math.max(...xs) + 280, y: right?.position?.y ?? 120 };
    }
    let newId = "";
    update((d) => {
      const r = addNode(d, spec, {
        position: at,
        ...(after && port && !isTrigger(spec.type) ? { after: { id: after.id, port } } : {}),
      });
      newId = r.id;
      return r.def;
    });
    if (newId) setSelected([newId]);
  }

  function deleteNode(nodeId: string) {
    update((d) => removeNodes(d, [nodeId]));
    setSelected([]);
  }

  function duplicateNode(nodeId: string) {
    let ids: string[] = [];
    update((d) => {
      const r = pasteFragment(d, fragmentOf(d, [nodeId]), { x: 40, y: 80 });
      ids = r.ids;
      return r.def;
    });
    if (ids.length) setSelected(ids);
  }

  function setPin(nodeId: string, items: Obj[] | null) {
    update((d) => {
      const pins = { ...(d.pins ?? {}) };
      if (items?.length) pins[nodeId] = items.slice(0, 20);
      else delete pins[nodeId];
      if (Object.keys(pins).length === 0) {
        const { pins: _gone, ...rest } = d;
        return rest;
      }
      return { ...d, pins };
    });
  }

  // ---- tests ----
  interface TestRequest {
    trigger_node_id?: string;
    trigger_item?: Obj;
    test_event?: Record<string, string | undefined>;
    only_node?: string;
  }

  /** Run the whole workflow from a trigger, or one node (`onlyNode`). A node that no trigger reaches runs alone. */
  async function startTest(onlyNode?: string, fromTrigger?: string) {
    if (busy || readOnly) return;
    const triggers = def.nodes.filter((n) => isTrigger(n.type));
    const reach = onlyNode ? new Set(upstream(def, onlyNode)) : null;
    const candidates = onlyNode ? triggers.filter((t) => reach?.has(t.id)) : triggers;
    if (onlyNode && candidates.length === 0 && !isTrigger(findNode(def, onlyNode)?.type ?? "")) {
      // no trigger leads here: the node runs alone with the data of the node before it
      const alone: TestRequest = { only_node: onlyNode };
      const missing = (def.variables ?? []).filter(
        (v) => v.required && (v.value === null || v.value === undefined || v.value === ""),
      );
      if (missing.length > 0) setAskVars({ body: alone });
      else await sendTest(alone, undefined);
      return;
    }
    if (triggers.length === 0) {
      toast.error("Add a trigger first.");
      return;
    }
    const picked =
      (fromTrigger ? candidates.find((t) => t.id === fromTrigger) : undefined) ??
      (selected.length === 1 ? candidates.find((t) => t.id === selected[0]) : undefined) ??
      candidates[0];
    if (!picked) {
      toast.error("No trigger leads to this node.");
      return;
    }
    const tb = testBody(picked, testInputs[picked.id]);
    if ("error" in tb) {
      setSelected([picked.id]);
      toast.error(tb.error);
      return;
    }
    const request: TestRequest = { ...tb, ...(onlyNode ? { only_node: onlyNode } : {}) };
    const missing = (def.variables ?? []).filter(
      (v) => v.required && (v.value === null || v.value === undefined || v.value === ""),
    );
    if (missing.length > 0) {
      setAskVars({ body: request });
      return;
    }
    await sendTest(request, undefined);
  }

  async function sendTest(request: TestRequest, variables: Obj | undefined) {
    setBusy(true);
    try {
      const saved = await draft.flush();
      if (!saved) {
        toast.error(
          "The draft is not saved yet. Fix the connection or the conflict, then try again.",
        );
        return;
      }
      const r = await call.test(id, {
        ...request,
        ...(realWrites ? { real_writes: true } : {}),
        ...(variables ? { variables } : {}),
      } as never);
      setShowBar(true);
      void live.watch(r.run_id);
    } catch (e) {
      if (e instanceof ApiProblem && e.code === "workflow_has_errors") {
        const v = e.details.validation as Validation | undefined;
        const first = v?.errors[0];
        toast.error(first ? first.message : "The workflow has errors.");
        if (first?.node_id) setSelected([first.node_id]);
      } else toast.error(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  async function listen(nodeId: string) {
    const saved = await draft.flush();
    if (!saved) {
      toast.error("The draft is not saved yet.");
      return;
    }
    try {
      const r = await call.listen(id, nodeId);
      setListening({ nodeId, until: r.until, url: r.test_url });
    } catch (e) {
      toast.error(messageOf(e));
    }
  }

  // while listening, look for the test run the webhook starts
  useEffect(() => {
    if (!listening) return;
    const since = Date.now() - 2000;
    const t = setInterval(async () => {
      if (Date.now() > new Date(listening.until).getTime() + 2000) {
        setListening(null);
        return;
      }
      try {
        const list = await call.listRuns(id, "test");
        const fresh = list.runs.find((r) => new Date(r.queued_at).getTime() >= since);
        if (fresh) {
          setListening(null);
          setShowBar(true);
          void live.watch(fresh.id);
        }
      } catch {
        // try again on the next tick
      }
    }, 2000);
    return () => clearInterval(t);
  }, [listening?.until]);

  // ---- keys ----
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const el = document.activeElement as HTMLElement | null;
      const typing =
        !!el &&
        (el.tagName === "INPUT" ||
          el.tagName === "TEXTAREA" ||
          el.tagName === "SELECT" ||
          el.isContentEditable);
      const mod = e.ctrlKey || e.metaKey;
      if (mod && e.key.toLowerCase() === "s") {
        e.preventDefault();
        draft.saveNow();
      } else if (mod && e.key === "Enter") {
        e.preventDefault();
        void startTest();
      } else if (!typing && mod && e.key.toLowerCase() === "z") {
        e.preventDefault();
        if (e.shiftKey) draft.redo();
        else draft.undo();
      } else if (!typing && mod && e.key.toLowerCase() === "y") {
        e.preventDefault();
        draft.redo();
      } else if (!typing && !mod && e.key === "/") {
        e.preventDefault();
        paletteInput.current?.focus();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  });

  async function downloadVersion(version: number) {
    await downloader.start({
      id,
      version,
      hasPins: async () =>
        Object.keys((await client.fetchQuery(aq.version(id, version))).definition.pins ?? {})
          .length > 0,
    });
  }

  const nodeName = (nodeId: string) => viewDef.nodes.find((n) => n.id === nodeId)?.name || nodeId;
  const goEditor = () => navigate({ to: "/w/$ws/automations/$id", params: { ws, id } });
  const goRuns = () => navigate({ to: "/w/$ws/automations/$id/executions", params: { ws, id } });
  const goRun = (rid: string) =>
    navigate({ to: "/w/$ws/automations/$id/executions/$runId", params: { ws, id, runId: rid } });

  const status0 = !meta.published
    ? "Draft"
    : `v${meta.published.version} published${changed ? ", draft changed" : ""}`;

  return (
    <EditorProvider
      value={{
        workflowId: id,
        def: viewDef,
        specs,
        issues,
        run: viewing || showBar ? run : null,
        dataRun,
        live: live.live,
        readOnly,
        saveTick,
        isAdmin,
      }}
    >
      <div className="flex h-full min-h-[480px] flex-col">
        <header className="flex flex-wrap items-center gap-x-3 gap-y-2 border-b border-border bg-surface px-3 py-2">
          <Link
            to="/w/$ws/automations"
            params={{ ws }}
            aria-label="Back to Automations"
            className="flex h-8 items-center gap-1 rounded-(--radius-control) px-2 text-sm text-muted hover:bg-surface-2 hover:text-text"
          >
            <ArrowLeft size={15} aria-hidden="true" /> Automations
          </Link>
          <input
            aria-label="Workflow name"
            value={def.name}
            disabled={readOnly}
            maxLength={120}
            onChange={(e) => update((d) => ({ ...d, name: e.target.value }), { key: "name" })}
            className="h-8 min-w-40 max-w-xs flex-1 rounded-(--radius-control) bg-transparent px-2 text-sm font-semibold hover:bg-surface-2 focus:bg-surface-2"
          />
          <span className="text-xs text-muted">{status0}</span>
          <SaveLabel state={draft.state} />
          <div className="flex w-full flex-wrap items-center gap-2">
            <nav aria-label="Views" className="flex gap-1">
              <TabLink active={tab === "editor"} onClick={goEditor}>
                Editor
              </TabLink>
              <TabLink active={tab === "executions"} onClick={goRuns}>
                Executions
              </TabLink>
            </nav>
            <div className="ml-auto flex flex-wrap items-center gap-2">
              {tab === "editor" && !readOnly && (
                <>
                  <IconBtn label="Undo (Ctrl+Z)" disabled={!draft.canUndo} onClick={draft.undo}>
                    <Undo2 size={15} />
                  </IconBtn>
                  <IconBtn
                    label="Redo (Ctrl+Shift+Z)"
                    disabled={!draft.canRedo}
                    onClick={draft.redo}
                  >
                    <Redo2 size={15} />
                  </IconBtn>
                  <label
                    className={cn(
                      "flex items-center gap-2 rounded-(--radius-control) border px-2.5 py-1 text-xs",
                      realWrites ? "border-danger/50 text-danger" : "border-border text-muted",
                    )}
                    title="Off: nodes that change data run dry in tests"
                  >
                    <input
                      type="checkbox"
                      role="switch"
                      aria-checked={realWrites}
                      checked={realWrites}
                      onChange={(e) => setRealWrites(e.target.checked)}
                      className="accent-(--accent)"
                    />
                    Real writes
                  </label>
                  <Button
                    variant="secondary"
                    className="h-8"
                    busy={busy}
                    onClick={() => void startTest()}
                    title="Ctrl+Enter"
                  >
                    <FlaskConical size={14} aria-hidden="true" /> Test workflow
                  </Button>
                  <Button className="h-8" onClick={() => setDialog("publish")}>
                    <Rocket size={14} aria-hidden="true" /> Publish
                  </Button>
                </>
              )}
              <label className="flex items-center gap-2 text-sm">
                <input
                  type="checkbox"
                  role="switch"
                  aria-checked={meta.active}
                  aria-label="Active"
                  checked={meta.active}
                  disabled={meta.archived}
                  onChange={() => {
                    if (meta.active) setDialog("off");
                    else if (!meta.published) {
                      toast("Publish a version first. Only a published version can be turned on.");
                      setDialog("publish");
                    } else setDialog("on");
                  }}
                  className="size-4 accent-(--accent)"
                />
                Active
              </label>
              <MenuRoot>
                <MenuTrigger
                  aria-label="More"
                  className="flex size-8 items-center justify-center rounded-(--radius-control) border border-border bg-surface hover:bg-surface-2"
                >
                  <MoreHorizontal size={16} />
                </MenuTrigger>
                <MenuContent align="end">
                  <MenuItem onSelect={() => setDialog("settings")}>
                    <Settings2 size={15} aria-hidden="true" /> Workflow settings
                  </MenuItem>
                  <MenuItem onSelect={() => setDialog("versions")}>
                    <History size={15} aria-hidden="true" /> Versions
                  </MenuItem>
                  <MenuItem
                    onSelect={() =>
                      void downloader.start({
                        id,
                        hasPins: () => Object.keys(def.pins ?? {}).length > 0,
                      })
                    }
                  >
                    <Download size={15} aria-hidden="true" /> Download the draft
                  </MenuItem>
                  {meta.published && (
                    <MenuItem onSelect={() => void downloadVersion(meta.published?.version ?? 0)}>
                      <Download size={15} aria-hidden="true" /> Download version{" "}
                      {meta.published.version}
                    </MenuItem>
                  )}
                  <MenuSeparator />
                  <MenuItem asChild>
                    <Link to="/w/$ws/automations/secrets" params={{ ws }}>
                      Secrets
                    </Link>
                  </MenuItem>
                </MenuContent>
              </MenuRoot>
            </div>
          </div>
        </header>

        {draft.state === "conflict" && (
          <div
            role="alert"
            className="flex flex-wrap items-center gap-3 border-b border-warning/40 bg-warning/10 px-4 py-2 text-sm"
          >
            Changed in another tab. Reload that version, or overwrite it with yours.
            <Button variant="secondary" className="h-7" onClick={draft.useTheirs}>
              Reload
            </Button>
            <Button variant="secondary" className="h-7" onClick={draft.overwrite}>
              Overwrite
            </Button>
          </div>
        )}
        {draft.state === "offline" && (
          <div
            role="status"
            className="border-b border-warning/40 bg-warning/10 px-4 py-1.5 text-sm"
          >
            No connection to the server. Your changes are kept here and saved again soon.
          </div>
        )}
        {meta.archived && (
          <div className="border-b border-border bg-surface-2 px-4 py-1.5 text-sm">
            This workflow is archived. Open the list to bring it back.
          </div>
        )}
        {engine.data?.paused && (
          <div className="border-b border-warning/40 bg-warning/10 px-4 py-1.5 text-sm">
            The engine is paused. Production runs wait. Test runs still run.
          </div>
        )}

        {report && tab === "editor" && (
          <ImportReportPanel
            ws={ws}
            report={report}
            def={def}
            update={update}
            onClose={() => setReport(null)}
            onSelectNode={(nid) => setSelected([nid])}
          />
        )}

        {tab === "executions" && !runId && (
          <RunsTable
            workflowId={id}
            mode={mode}
            status={status}
            onMode={setMode}
            onStatus={setStatus}
            onOpen={goRun}
          />
        )}

        {(tab === "editor" || runId) && (
          <>
            {viewing && run && (
              <RunStrip
                run={run}
                onBack={goRuns}
                onRetried={goRun}
                onCopyToEditor={() => {
                  update((d) => ({
                    ...d,
                    pins: { ...(d.pins ?? {}), [run.trigger_node_id]: [run.trigger_item] },
                  }));
                  toast.success("The trigger item is pinned. Test the workflow to use it.");
                  void goEditor();
                }}
              />
            )}
            <div className="flex min-h-0 flex-1">
              {tab === "editor" && !readOnly && (
                <div className="hidden w-60 shrink-0 md:block">
                  <Palette ref={paletteInput} specs={specList} onPick={addFromPalette} />
                </div>
              )}
              <div className="relative min-w-0 flex-1">
                <Canvas
                  update={update}
                  mark={draft.mark}
                  selected={selected}
                  onSelect={setSelected}
                  onAdded={(nid) => setSelected([nid])}
                  onPasteWorkflow={(t) => void pasteWorkflow(t)}
                  onRunNode={(nid) => {
                    const n = findNode(def, nid);
                    if (n && isTrigger(n.type)) void startTest(undefined, nid);
                    else void startTest(nid);
                  }}
                  runBusy={busy}
                />
              </div>
              {panelNode && (
                <div className="fixed inset-0 z-40 bg-surface md:static md:z-auto md:w-[420px] md:shrink-0">
                  <NodePanel
                    key={panelNode.id}
                    node={panelNode}
                    spec={panelSpec}
                    onParams={(pp) => setParams(panelNode.id, pp)}
                    onPatch={(pt) => patch(panelNode.id, pt)}
                    onRename={(nid) => {
                      update((d) => renameNode(d, panelNode.id, nid));
                      setSelected([nid]);
                    }}
                    onDelete={() => deleteNode(panelNode.id)}
                    onDuplicate={() => duplicateNode(panelNode.id)}
                    onClose={() => setSelected([])}
                    onTestStep={() => void startTest(panelNode.id)}
                    onPin={(items) => setPin(panelNode.id, items)}
                    testInput={testInputs[panelNode.id]}
                    onTestInput={(pt) =>
                      setTestInputs((t) => ({
                        ...t,
                        [panelNode.id]: { ...t[panelNode.id], ...pt },
                      }))
                    }
                    listening={listening}
                    onListen={() => void listen(panelNode.id)}
                    busy={busy}
                  />
                </div>
              )}
            </div>
            {tab === "editor" && showBar && (
              <TestBar
                run={live.run}
                live={live.live}
                error={live.error}
                nodeName={nodeName}
                onOpen={() => live.runId && goRun(live.runId)}
                onCancel={() =>
                  live.runId && void call.cancel(live.runId).catch((e) => toast.error(messageOf(e)))
                }
                onClose={() => {
                  setShowBar(false);
                  live.clear();
                }}
                onSelectNode={(nid) => setSelected([nid])}
              />
            )}
          </>
        )}
      </div>

      {downloader.dialog}
      <PasteWorkflowDialog
        checking={pasting.checking}
        result={pasting.result}
        onChoose={choosePaste}
      />
      <PublishDialog
        open={dialog === "publish"}
        onClose={() => setDialog(null)}
        workflowId={id}
        def={def}
        published={meta.published?.version ?? null}
        onSelectNode={(nid) => {
          if (tab !== "editor") void goEditor();
          setSelected([nid]);
        }}
        onDone={(w) => {
          setMeta((m) => ({ ...m, published: w.published, active: w.active }));
          void client.invalidateQueries({ queryKey: ["automations", "versions", id] });
          void client.invalidateQueries({ queryKey: ["automations", "list"] });
        }}
      />
      <ActiveDialog
        open={dialog === "on" || dialog === "off"}
        mode={dialog === "off" ? "off" : "on"}
        onClose={() => setDialog(null)}
        workflowId={id}
        published={meta.published?.version ?? null}
        onDone={(w) => {
          setMeta((m) => ({ ...m, active: w.active, published: w.published }));
          void client.invalidateQueries({ queryKey: ["automations", "list"] });
          void client.invalidateQueries({ queryKey: ["automations", "status"] });
        }}
      />
      <WorkflowSettings
        open={dialog === "settings"}
        onClose={() => setDialog(null)}
        def={def}
        onSave={(pt) =>
          update((d) => ({
            ...d,
            description: pt.description ?? "",
            settings: pt.settings ?? {},
            variables: pt.variables ?? [],
          }))
        }
      />
      <VersionsDialog
        open={dialog === "versions"}
        onClose={() => setDialog(null)}
        workflowId={id}
        onDownload={(v) => void downloadVersion(v)}
        onRestored={(w) => {
          draft.adopt({ draft: w.draft, draft_hash: w.draft_hash, validation: w.validation });
          setSelected([]);
          toast.success("The version is in the draft. Publish it to use it.");
        }}
      />
      <AskVariables
        open={askVars !== null}
        onClose={() => setAskVars(null)}
        variables={(def.variables ?? []).filter(
          (v) => v.required && (v.value === null || v.value === undefined || v.value === ""),
        )}
        onRun={(values) => {
          const body = askVars?.body;
          setAskVars(null);
          if (body) void sendTest(body, values);
        }}
      />
    </EditorProvider>
  );
}

function SaveLabel({ state }: { state: ReturnType<typeof useDraft>["state"] }) {
  const text = {
    saved: "Saved",
    saving: "Saving…",
    dirty: "Unsaved changes",
    offline: "Offline",
    conflict: "Conflict",
  }[state];
  return (
    <span
      className={cn(
        "text-xs",
        state === "offline" || state === "conflict" ? "text-warning" : "text-muted",
      )}
      aria-live="polite"
    >
      {text}
    </span>
  );
}

function TabLink({
  active,
  onClick,
  children,
}: {
  active: boolean;
  onClick: () => void;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-current={active ? "page" : undefined}
      onClick={onClick}
      className={cn(
        "rounded-(--radius-control) px-3 py-1 text-sm",
        active ? "bg-surface-2 font-medium" : "text-muted hover:bg-surface-2 hover:text-text",
      )}
    >
      {children}
    </button>
  );
}

function IconBtn({
  label,
  onClick,
  children,
  disabled,
}: {
  label: string;
  onClick: () => void;
  children: ReactNode;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      disabled={disabled}
      onClick={onClick}
      className="flex size-8 items-center justify-center rounded-(--radius-control) text-muted hover:bg-surface-2 hover:text-text disabled:opacity-40"
    >
      {children}
    </button>
  );
}

function VersionsDialog({
  open,
  onClose,
  workflowId,
  onRestored,
  onDownload,
}: {
  open: boolean;
  onClose: () => void;
  workflowId: string;
  onRestored: (w: WorkflowDetail) => void;
  onDownload: (version: number) => void;
}) {
  const versions = useQuery({ ...aq.versions(workflowId), enabled: open });
  const [busy, setBusy] = useState<number | null>(null);
  return (
    <Modal open={open} onClose={onClose} title="Versions" size="md">
      {versions.isPending && <Skeleton rows={3} />}
      {versions.isError && <ErrorNote error={versions.error} />}
      {versions.data?.length === 0 && (
        <p className="text-sm text-muted">Nothing is published yet.</p>
      )}
      <ul className="flex flex-col gap-2">
        {versions.data?.map((v) => (
          <li
            key={v.version}
            className="flex items-center justify-between gap-3 rounded-(--radius-control) border border-border px-3 py-2"
          >
            <div className="min-w-0">
              <p className="text-sm font-medium">
                Version {v.version} {v.is_published && <Pill tone="success">live</Pill>}
              </p>
              <p className="truncate text-xs text-muted">
                {fmtDateTime(v.published_at)}
                {v.note ? ` · ${v.note}` : ""}
              </p>
            </div>
            <span className="flex shrink-0 gap-2">
              <Button variant="secondary" className="h-8" onClick={() => onDownload(v.version)}>
                Download
              </Button>
              <Button
                variant="secondary"
                className="h-8"
                busy={busy === v.version}
                onClick={async () => {
                  setBusy(v.version);
                  try {
                    onRestored(await call.restoreVersion(workflowId, v.version));
                    onClose();
                  } catch (e) {
                    toast.error(messageOf(e));
                  } finally {
                    setBusy(null);
                  }
                }}
              >
                Restore to draft
              </Button>
            </span>
          </li>
        ))}
      </ul>
      <p className="mt-3 text-xs text-muted">
        Restoring copies the version into the draft. It does not publish. The old draft stays in the
        draft history.
      </p>
    </Modal>
  );
}
