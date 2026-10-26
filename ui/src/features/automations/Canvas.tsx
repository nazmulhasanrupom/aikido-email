import {
  applyNodeChanges,
  Background,
  BackgroundVariant,
  BaseEdge,
  Controls,
  EdgeLabelRenderer,
  type EdgeProps,
  getBezierPath,
  Handle,
  MiniMap,
  type Node,
  type NodeChange,
  type NodeProps,
  NodeResizer,
  type OnConnectEnd,
  Panel,
  Position,
  ReactFlow,
  ReactFlowProvider,
  SelectionMode,
  useReactFlow,
} from "@xyflow/react";
import "@xyflow/react/dist/base.css";
import {
  AlignHorizontalSpaceAround,
  Copy,
  Loader2,
  Play,
  Power,
  StickyNote,
  Trash2,
  X,
} from "lucide-react";
import {
  createContext,
  memo,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { cn } from "@/lib/cn";
import { useEditor } from "./ctx";
import {
  addNode,
  connect,
  connectProblem,
  fragmentFromText,
  fragmentOf,
  insertOnEdge,
  isTrigger,
  outPorts,
  pasteFragment,
  portLabel,
  portOf,
  removeNodes,
  uniqueId,
} from "./defs";
import { NODE_H, NODE_W, withTidyPositions } from "./layout";
import { CATEGORY_ICON, iconFor } from "./meta";
import { DRAG_TYPE, QuickAdd } from "./Palette";
import { isWholeWorkflow } from "./transfer";
import type { NodeSpec, WfDef, WfEdge } from "./types";
import { nodeRow } from "./useRun";

const NOTE_PREFIX = "note:";

// ---------- the node card ----------

const RING: Record<string, string> = {
  running: "ring-2 ring-info animate-pulse",
  success: "ring-2 ring-success/70",
  dry: "ring-2 ring-success/40",
  error: "ring-2 ring-danger",
  waiting: "ring-2 ring-warning",
  skipped: "opacity-60",
};

/** What the buttons over a node do. Set by the canvas. */
interface NodeActions {
  run: (id: string) => void;
  toggle: (id: string) => void;
  duplicate: (id: string) => void;
  remove: (id: string) => void;
  /// A test is running.
  busy: boolean;
}
const NodeActionsCtx = createContext<NodeActions>({
  run: () => {},
  toggle: () => {},
  duplicate: () => {},
  remove: () => {},
  busy: false,
});

function BarBtn({
  label,
  onClick,
  disabled,
  danger,
  children,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
  danger?: boolean;
  children: React.ReactNode;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      title={label}
      disabled={disabled}
      onClick={(e) => {
        e.stopPropagation();
        onClick();
      }}
      onDoubleClick={(e) => e.stopPropagation()}
      className={cn(
        "nodrag nopan flex size-7 items-center justify-center rounded-md text-muted hover:bg-surface-2 disabled:opacity-40",
        danger ? "hover:text-danger" : "hover:text-text",
      )}
    >
      {children}
    </button>
  );
}

const WfNodeCard = memo(function WfNodeCard({ id, selected }: NodeProps) {
  const ed = useEditor();
  const act = useContext(NodeActionsCtx);
  const node = ed.def.nodes.find((n) => n.id === id);
  if (!node) return null;
  const spec = ed.specs.get(node.type);
  const Icon = iconFor(node.type);
  const trigger = isTrigger(node.type);
  const ports = outPorts(node, spec);
  const row = nodeRow(ed.run, id);
  const issues = ed.issues.get(id) ?? [];
  const firstError = issues.find((i) => i.level === "error");
  const pinned = !!ed.def.pins?.[id]?.length;
  return (
    <div
      className={cn(
        "group relative w-55 border bg-surface text-left shadow-(--shadow-card)",
        trigger ? "rounded-r-[10px] rounded-l-sm" : "rounded-[10px]",
        selected ? "border-accent" : "border-border",
        node.disabled && "opacity-50",
        row && RING[row.status],
      )}
      style={{ minHeight: Math.max(NODE_H - 8, ports.length * 22 + 14) }}
    >
      {!trigger && <Handle type="target" position={Position.Left} id="in" className="wf-handle" />}
      {!ed.readOnly && (
        <div
          className={cn(
            "absolute bottom-full left-1/2 z-10 -translate-x-1/2 pb-1.5 transition-opacity group-focus-within:opacity-100 group-hover:opacity-100",
            selected ? "opacity-100" : "opacity-0",
          )}
        >
          <div className="flex items-center gap-0.5 rounded-lg border border-border bg-surface p-0.5 shadow-(--shadow-card)">
            <BarBtn
              label={trigger ? "Run the workflow from here" : "Run this node"}
              disabled={act.busy || node.disabled === true}
              onClick={() => act.run(id)}
            >
              {act.busy ? (
                <Loader2 size={14} className="animate-spin" aria-hidden="true" />
              ) : (
                <Play size={14} aria-hidden="true" />
              )}
            </BarBtn>
            <BarBtn
              label={node.disabled ? "Turn the node on" : "Turn the node off"}
              onClick={() => act.toggle(id)}
            >
              <Power size={14} aria-hidden="true" />
            </BarBtn>
            <BarBtn label="Duplicate" onClick={() => act.duplicate(id)}>
              <Copy size={14} aria-hidden="true" />
            </BarBtn>
            <BarBtn label="Delete" danger onClick={() => act.remove(id)}>
              <Trash2 size={14} aria-hidden="true" />
            </BarBtn>
          </div>
        </div>
      )}
      <div className="flex items-center gap-2.5 p-2.5">
        <span
          className={cn(
            "flex size-8 shrink-0 items-center justify-center rounded-lg",
            CATEGORY_ICON[spec?.category ?? "flow"],
          )}
        >
          <Icon size={16} aria-hidden="true" />
        </span>
        <div className="min-w-0 flex-1">
          <p className="truncate text-sm font-semibold">{node.name || spec?.label || node.type}</p>
          <p className="truncate text-xs text-muted">{spec?.label ?? node.type}</p>
        </div>
      </div>
      <div className="absolute -top-2 right-2 flex gap-1">
        {node.disabled && <Tag>disabled</Tag>}
        {row?.status === "dry" && <Tag>dry</Tag>}
        {pinned && <Tag>pinned</Tag>}
        {row?.status === "waiting" && <Tag>waiting</Tag>}
      </div>
      {firstError && (
        <span
          role="img"
          aria-label={`Error: ${firstError.message}`}
          title={firstError.message}
          className="absolute -top-1 -left-1 size-3 rounded-full bg-danger ring-2 ring-surface"
        />
      )}
      {ports.map((p, i) => {
        const top = `${((i + 0.5) / ports.length) * 100}%`;
        return (
          <div key={p}>
            <Handle
              type="source"
              position={Position.Right}
              id={p}
              style={{ top }}
              className="wf-handle"
            />
            {ports.length > 1 && (
              <span
                className="pointer-events-none absolute right-3 -translate-y-1/2 text-[10px] text-muted"
                style={{ top }}
              >
                {portLabel(p) || "main"}
              </span>
            )}
          </div>
        );
      })}
    </div>
  );
});

function Tag({ children }: { children: React.ReactNode }) {
  return (
    <span className="rounded-full border border-border bg-surface px-1.5 text-[10px] leading-4 text-muted">
      {children}
    </span>
  );
}

// ---------- sticky notes ----------

const NoteSetter = createContext<(id: string, patch: { text?: string }) => void>(() => {});
const EdgeDeleter = createContext<(e: { from: string; port: string; to: string }) => void>(
  () => {},
);

const NoteCard = memo(function NoteCard({ id, selected }: NodeProps) {
  const ed = useEditor();
  const noteId = id.slice(NOTE_PREFIX.length);
  const note = ed.def.notes?.find((n) => n.id === noteId);
  const set = useContext(NoteSetter);
  if (!note) return null;
  return (
    <div className="h-full w-full rounded-md border border-warning/30 bg-warning/15 p-2 shadow-(--shadow-card)">
      <NodeResizer isVisible={selected && !ed.readOnly} minWidth={120} minHeight={60} />
      <textarea
        aria-label="Note"
        value={note.text}
        readOnly={ed.readOnly}
        placeholder="Write a note…"
        onChange={(e) => set(noteId, { text: e.target.value })}
        className="nodrag nowheel h-full w-full resize-none bg-transparent text-xs outline-none"
      />
    </div>
  );
});

// ---------- the edge ----------

interface EdgeData {
  count?: number;
  live?: boolean;
  from: string;
  port: string;
  to: string;
  [key: string]: unknown;
}

const WfEdgeView = memo(function WfEdgeView({
  id,
  sourceX,
  sourceY,
  targetX,
  targetY,
  sourcePosition,
  targetPosition,
  selected,
  data,
  markerEnd,
}: EdgeProps) {
  const ed = useEditor();
  const del = useContext(EdgeDeleter);
  const [path, lx, ly] = getBezierPath({
    sourceX,
    sourceY,
    targetX,
    targetY,
    sourcePosition,
    targetPosition,
  });
  const d = data as EdgeData | undefined;
  // the cross shows while the pointer is on the line or on the cross (a short delay lets it cross the gap)
  const [hover, setHover] = useState(false);
  const leave = useRef<number | undefined>(undefined);
  const enter = () => {
    window.clearTimeout(leave.current);
    setHover(true);
  };
  const out = () => {
    leave.current = window.setTimeout(() => setHover(false), 150);
  };
  useEffect(() => () => window.clearTimeout(leave.current), []);
  const canCut = !ed.readOnly && !!d && (hover || selected);
  return (
    <>
      {/* biome-ignore lint/a11y/noStaticElementInteractions: hover only; the cross is a real button */}
      <g onMouseEnter={enter} onMouseLeave={out}>
        <BaseEdge
          id={id}
          path={path}
          {...(markerEnd ? { markerEnd } : {})}
          className={d?.live ? "wf-edge-live" : ""}
          {...(canCut && hover ? { style: { stroke: "var(--danger)" } } : {})}
          interactionWidth={24}
        />
      </g>
      <EdgeLabelRenderer>
        {/* biome-ignore lint/a11y/noStaticElementInteractions: hover only; the cross is a real button */}
        <div
          className="nodrag nopan absolute flex items-center gap-1"
          style={{
            transform: `translate(-50%, -50%) translate(${lx}px, ${ly}px)`,
            pointerEvents: "all",
          }}
          onMouseEnter={enter}
          onMouseLeave={out}
        >
          {d?.count !== undefined && (
            <span className="rounded-full border border-border bg-surface px-1.5 text-[10px] leading-4 text-muted">
              {d.count} {d.count === 1 ? "item" : "items"}
            </span>
          )}
          {canCut && d && (
            <button
              type="button"
              aria-label="Cut the line"
              title="Cut the line"
              onClick={() => del({ from: d.from, port: d.port, to: d.to })}
              className="flex size-6 items-center justify-center rounded-full border border-danger bg-surface text-danger shadow-(--shadow-button) hover:bg-danger hover:text-white"
            >
              <X size={14} aria-hidden="true" />
            </button>
          )}
        </div>
      </EdgeLabelRenderer>
    </>
  );
});

const nodeTypes = { wf: WfNodeCard, note: NoteCard };
const DELETE_KEYS = ["Backspace", "Delete"];
const MULTI_KEYS = ["Shift", "Meta", "Control"];
const PAN_BUTTONS = [1, 2];
const FIT = { padding: 0.25, maxZoom: 1 };
const edgeTypes = { wf: WfEdgeView };

// ---------- the canvas ----------

export interface CanvasProps {
  update: (fn: (d: WfDef) => WfDef, opts?: { record?: boolean }) => void;
  mark: () => void;
  selected: string[];
  onSelect: (ids: string[]) => void;
  /** Called with the new node's id so the panel can open on it. */
  onAdded: (id: string) => void;
  /** Called with the text of a whole workflow file that was pasted (not the node clipboard). */
  onPasteWorkflow?: (text: string) => void;
  /** Run one node (a trigger starts the whole workflow). */
  onRunNode: (id: string) => void;
  /** A test is running. */
  runBusy: boolean;
}

function isTyping(el: Element | null): boolean {
  if (!el) return false;
  const tag = el.tagName;
  return (
    tag === "INPUT" ||
    tag === "TEXTAREA" ||
    tag === "SELECT" ||
    (el as HTMLElement).isContentEditable
  );
}

/** A touch screen cannot hold a key or press the middle button, so it keeps the simple drag-to-pan mode. */
function useCoarsePointer(): boolean {
  const [coarse, setCoarse] = useState(
    () => typeof window !== "undefined" && window.matchMedia?.("(pointer: coarse)").matches === true,
  );
  useEffect(() => {
    const m = window.matchMedia?.("(pointer: coarse)");
    if (!m) return;
    const on = () => setCoarse(m.matches);
    m.addEventListener("change", on);
    return () => m.removeEventListener("change", on);
  }, []);
  return coarse;
}

function Flow({
  update,
  mark,
  selected,
  onSelect,
  onAdded,
  onPasteWorkflow,
  onRunNode,
  runBusy,
}: CanvasProps) {
  const ed = useEditor();
  const rf = useReactFlow();
  const box = useRef<HTMLDivElement>(null);
  const [nodes, setNodes] = useState<Node[]>([]);
  const [quick, setQuick] = useState<{
    x: number;
    y: number;
    at: { x: number; y: number };
    from: { id: string; port: string };
  } | null>(null);
  const sel = useMemo(() => new Set(selected), [selected]);
  const specs = ed.specs;
  const selRef = useRef(sel);
  selRef.current = sel;
  const specList = useMemo(() => [...specs.values()], [specs]);
  const coarse = useCoarsePointer();

  // the definition is the truth; the flow state only adds what a drag needs
  useEffect(() => {
    const next: Node[] = ed.def.nodes.map((n) => ({
      id: n.id,
      type: "wf",
      position: n.position ?? { x: 0, y: 0 },
      data: {},
      selected: sel.has(n.id),
      draggable: !ed.readOnly,
      connectable: !ed.readOnly,
    }));
    for (const note of ed.def.notes ?? []) {
      next.push({
        id: `${NOTE_PREFIX}${note.id}`,
        type: "note",
        position: note.position,
        data: {},
        width: note.size?.width ?? 200,
        height: note.size?.height ?? 120,
        selected: sel.has(`${NOTE_PREFIX}${note.id}`),
        draggable: !ed.readOnly,
        connectable: false,
        zIndex: -1,
      });
    }
    setNodes(next);
  }, [ed.def.nodes, ed.def.notes, sel, ed.readOnly]);

  const edges = useMemo(
    () =>
      ed.def.edges.map((e) => {
        const p = portOf(e);
        const from = nodeRow(ed.run, e.from);
        const count = from?.output_count?.[p];
        const target = nodeRow(ed.run, e.to);
        const data: EdgeData = {
          from: e.from,
          port: p,
          to: e.to,
          ...(count !== undefined ? { count } : {}),
          live: ed.live && target?.status === "running",
        };
        return {
          id: `${e.from}|${p}|${e.to}`,
          source: e.from,
          sourceHandle: p,
          target: e.to,
          targetHandle: "in",
          type: "wf",
          data,
          deletable: !ed.readOnly,
        };
      }),
    [ed.def.edges, ed.run, ed.live, ed.readOnly],
  );

  const onNodesChange = useCallback(
    (changes: NodeChange[]) => {
      setNodes((ns) => applyNodeChanges(changes, ns));
      // The selection comes from what the person does (a click, a box), never from our own writes back to the flow.
      const picks = changes.filter((c) => c.type === "select");
      if (picks.length > 0) {
        const cur = new Set(selRef.current);
        for (const c of picks) {
          if (c.type !== "select") continue;
          if (c.selected) cur.add(c.id);
          else cur.delete(c.id);
        }
        onSelect([...cur]);
      }
      // a resized note
      for (const c of changes) {
        if (
          c.type === "dimensions" &&
          c.resizing === false &&
          c.dimensions &&
          c.id.startsWith(NOTE_PREFIX)
        ) {
          const id = c.id.slice(NOTE_PREFIX.length);
          const size = {
            width: Math.round(c.dimensions.width),
            height: Math.round(c.dimensions.height),
          };
          update(
            (d) => ({
              ...d,
              notes: (d.notes ?? []).map((n) => (n.id === id ? { ...n, size } : n)),
            }),
            { record: false },
          );
        }
      }
    },
    [update, onSelect],
  );

  const setNote = useCallback(
    (id: string, patch: { text?: string }) =>
      update(
        (d) => ({
          ...d,
          notes: (d.notes ?? []).map((n) => (n.id === id ? { ...n, ...patch } : n)),
        }),
        { record: false },
      ),
    [update],
  );

  const deleteEdge = useCallback(
    (e: { from: string; port: string; to: string }) =>
      update((d) => ({
        ...d,
        edges: d.edges.filter((x) => !(x.from === e.from && portOf(x) === e.port && x.to === e.to)),
      })),
    [update],
  );

  function addNoteHere() {
    const rect = box.current?.getBoundingClientRect();
    const at = rf.screenToFlowPosition({
      x: (rect?.left ?? 0) + (rect?.width ?? 400) / 2,
      y: (rect?.top ?? 0) + (rect?.height ?? 300) / 2,
    });
    update((d) => ({
      ...d,
      notes: [
        ...(d.notes ?? []),
        {
          id: uniqueId(
            "note",
            (d.notes ?? []).map((n) => n.id),
          ),
          text: "",
          position: { x: Math.round(at.x), y: Math.round(at.y) },
          size: { width: 200, height: 120 },
        },
      ],
    }));
  }

  const nodeIds = (ids: string[]) => ids.filter((s) => !s.startsWith(NOTE_PREFIX));

  function removeSelection(ids: string[]) {
    const real = nodeIds(ids);
    const notes = ids.filter((i) => i.startsWith(NOTE_PREFIX)).map((i) => i.slice(NOTE_PREFIX.length));
    update((d) => {
      let next = real.length ? removeNodes(d, real) : d;
      if (notes.length)
        next = { ...next, notes: (next.notes ?? []).filter((n) => !notes.includes(n.id)) };
      return next;
    });
    onSelect([]);
  }

  function duplicateNodes(ids: string[]) {
    const real = nodeIds(ids);
    if (real.length === 0) return;
    let made: string[] = [];
    update((d) => {
      const r = pasteFragment(d, fragmentOf(d, real));
      made = r.ids;
      return r.def;
    });
    if (made.length) onSelect(made);
  }

  function toggleNodes(ids: string[]) {
    const real = new Set(nodeIds(ids));
    if (real.size === 0) return;
    update((d) => {
      // all on -> all off; else all on
      const off = d.nodes.filter((n) => real.has(n.id)).every((n) => !n.disabled);
      return {
        ...d,
        nodes: d.nodes.map((n) => (real.has(n.id) ? { ...n, disabled: off } : n)),
      };
    });
  }

  const actions = useMemo<NodeActions>(
    () => ({
      run: onRunNode,
      toggle: (id) => toggleNodes(selRef.current.has(id) ? [...selRef.current] : [id]),
      duplicate: (id) => duplicateNodes(selRef.current.has(id) ? [...selRef.current] : [id]),
      remove: (id) => removeSelection(selRef.current.has(id) ? [...selRef.current] : [id]),
      busy: runBusy,
    }),
    [onRunNode, runBusy, update, onSelect],
  );

  // copy, cut and paste (the browser's own events, so they work with the system clipboard)
  useEffect(() => {
    if (ed.readOnly) return;
    const textOf = (ids: string[]) => JSON.stringify(fragmentOf(ed.def, ids), null, 2);
    const onCopy = (e: ClipboardEvent) => {
      if (isTyping(document.activeElement) || window.getSelection()?.toString()) return;
      const ids = nodeIds(selected);
      if (ids.length === 0) return;
      e.clipboardData?.setData("text/plain", textOf(ids));
      e.preventDefault();
    };
    const onCut = (e: ClipboardEvent) => {
      if (isTyping(document.activeElement) || window.getSelection()?.toString()) return;
      const ids = nodeIds(selected);
      if (ids.length === 0) return;
      e.clipboardData?.setData("text/plain", textOf(ids));
      e.preventDefault();
      removeSelection(ids);
    };
    const onPaste = (e: ClipboardEvent) => {
      if (isTyping(document.activeElement)) return;
      const text = e.clipboardData?.getData("text/plain") ?? "";
      if (onPasteWorkflow && isWholeWorkflow(text)) {
        e.preventDefault();
        onPasteWorkflow(text);
        return;
      }
      const frag = fragmentFromText(text);
      if (!frag) return;
      e.preventDefault();
      let ids: string[] = [];
      update((d) => {
        const r = pasteFragment(d, frag);
        ids = r.ids;
        return r.def;
      });
      if (ids.length) onSelect(ids);
    };
    document.addEventListener("copy", onCopy);
    document.addEventListener("cut", onCut);
    document.addEventListener("paste", onPaste);
    return () => {
      document.removeEventListener("copy", onCopy);
      document.removeEventListener("cut", onCut);
      document.removeEventListener("paste", onPaste);
    };
  }, [ed.def, ed.readOnly, selected, update, onSelect, onPasteWorkflow]);

  // keys, as in n8n: Ctrl+A select all, Ctrl+D duplicate, D turn off or on, Esc clear, Delete
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (isTyping(document.activeElement) || e.defaultPrevented) return;
      // a dialog or the node panel's own controls are not the canvas
      if ((document.activeElement as HTMLElement | null)?.closest?.("[role=dialog]")) return;
      const mod = e.ctrlKey || e.metaKey;
      const key = e.key.toLowerCase();
      if (mod && key === "a") {
        e.preventDefault();
        onSelect([
          ...ed.def.nodes.map((n) => n.id),
          ...(ed.def.notes ?? []).map((n) => `${NOTE_PREFIX}${n.id}`),
        ]);
      } else if (!ed.readOnly && mod && key === "d") {
        e.preventDefault();
        duplicateNodes(selected);
      } else if (!ed.readOnly && !mod && !e.altKey && !e.shiftKey && key === "d") {
        toggleNodes(selected);
      } else if (!mod && key === "escape") {
        if (selected.length) onSelect([]);
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [ed.def, ed.readOnly, selected, update, onSelect]);

  const onConnectEnd: OnConnectEnd = (event, state) => {
    if (ed.readOnly || state.isValid || !state.fromNode || state.fromHandle?.type !== "source")
      return;
    const pt = "changedTouches" in event ? event.changedTouches[0] : event;
    if (!pt) return;
    const rect = box.current?.getBoundingClientRect();
    setQuick({
      x: Math.min(pt.clientX - (rect?.left ?? 0), (rect?.width ?? 600) - 300),
      y: Math.min(pt.clientY - (rect?.top ?? 0), (rect?.height ?? 500) - 340),
      at: rf.screenToFlowPosition({ x: pt.clientX, y: pt.clientY }),
      from: { id: state.fromNode.id, port: state.fromHandle?.id ?? "main" },
    });
  };

  function addFromQuick(spec: NodeSpec) {
    if (!quick) return;
    let newId = "";
    update((d) => {
      const r = addAt(d, spec, quick.at, quick.from);
      newId = r.id;
      return r.def;
    });
    setQuick(null);
    if (newId) onAdded(newId);
  }

  function onDrop(e: React.DragEvent) {
    e.preventDefault();
    const type = e.dataTransfer.getData(DRAG_TYPE);
    const spec = specs.get(type);
    if (!spec || ed.readOnly) return;
    const at = rf.screenToFlowPosition({ x: e.clientX, y: e.clientY });
    // dropped on a line: put the node in the middle of it
    const edgeEl = document
      .elementsFromPoint(e.clientX, e.clientY)
      .map((el) => el.closest(".react-flow__edge"))
      .find(Boolean);
    const eid = edgeEl?.getAttribute("data-id");
    let newId = "";
    update((d) => {
      const r = addAt(d, spec, { x: at.x - NODE_W / 2, y: at.y - NODE_H / 2 });
      newId = r.id;
      if (eid && !isTrigger(spec.type)) {
        const [from, port, to] = eid.split("|");
        const edge = d.edges.find((x) => x.from === from && portOf(x) === port && x.to === to);
        if (edge) return insertOnEdge(r.def, edge, r.id);
      }
      return r.def;
    });
    if (newId) onAdded(newId);
  }

  const tidy = () => update((d) => withTidyPositions(d));

  return (
    // biome-ignore lint/a11y/noStaticElementInteractions: a drop target for nodes dragged from the list
    <div
      ref={box}
      className="relative h-full w-full"
      onDragOver={(e) => e.preventDefault()}
      onDrop={onDrop}
    >
      <NodeActionsCtx.Provider value={actions}>
      <NoteSetter.Provider value={setNote}>
        <EdgeDeleter.Provider value={deleteEdge}>
          <ReactFlow
            nodes={nodes}
            edges={edges}
            nodeTypes={nodeTypes}
            edgeTypes={edgeTypes}
            onNodesChange={onNodesChange}
            onNodeDragStart={() => !ed.readOnly && mark()}
            onNodeDragStop={(_, __, dragged) => {
              const moved = new Map(dragged.map((n) => [n.id, n.position]));
              update(
                (d) => ({
                  ...d,
                  nodes: d.nodes.map((n) => {
                    const p = moved.get(n.id);
                    return p ? { ...n, position: { x: Math.round(p.x), y: Math.round(p.y) } } : n;
                  }),
                  notes: (d.notes ?? []).map((n) => {
                    const p = moved.get(`${NOTE_PREFIX}${n.id}`);
                    return p ? { ...n, position: { x: Math.round(p.x), y: Math.round(p.y) } } : n;
                  }),
                }),
                { record: false },
              );
            }}
            onConnect={(c) => {
              if (c.source && c.target)
                update((d) => connect(d, c.source, c.sourceHandle ?? "main", c.target));
            }}
            isValidConnection={(c) =>
              !!c.source &&
              !!c.target &&
              connectProblem(ed.def, c.source, c.sourceHandle ?? "main", c.target) === null
            }
            onConnectEnd={onConnectEnd}
            onNodesDelete={(del) => removeSelection(del.map((n) => n.id))}
            onEdgesDelete={(del) => {
              const gone = new Set(del.map((e) => e.id));
              update((d) => ({
                ...d,
                edges: d.edges.filter((e: WfEdge) => !gone.has(`${e.from}|${portOf(e)}|${e.to}`)),
              }));
            }}
            onPaneClick={() => setQuick(null)}
            onPaneContextMenu={(e) => e.preventDefault()}
            nodesDraggable={!ed.readOnly}
            nodesConnectable={!ed.readOnly}
            elementsSelectable
            deleteKeyCode={ed.readOnly ? null : DELETE_KEYS}
            multiSelectionKeyCode={MULTI_KEYS}
            // a mouse, as in n8n: drag on empty space draws a box, the middle or right button (or
            // Space + drag) moves the canvas, the wheel moves it, Ctrl + wheel zooms
            selectionOnDrag={!coarse}
            selectionMode={SelectionMode.Partial}
            panOnDrag={coarse ? true : PAN_BUTTONS}
            panOnScroll={!coarse}
            zoomOnScroll={coarse}
            zoomOnDoubleClick={false}
            panActivationKeyCode="Space"
            fitView
            fitViewOptions={FIT}
            minZoom={0.2}
            maxZoom={1.8}
            className="wf-canvas"
          >
            <Background variant={BackgroundVariant.Dots} gap={20} size={1.2} />
            <Controls showInteractive={false} position="bottom-left" />
            <MiniMap pannable zoomable className="wf-minimap" nodeStrokeWidth={2} />
            {!ed.readOnly && (
              <Panel position="bottom-center" className="flex gap-1">
                <button
                  type="button"
                  onClick={tidy}
                  className="inline-flex h-8 items-center gap-1.5 rounded-(--radius-control) border border-border bg-surface px-2.5 text-xs shadow-(--shadow-button) hover:bg-surface-2"
                >
                  <AlignHorizontalSpaceAround size={14} aria-hidden="true" /> Tidy
                </button>
                <button
                  type="button"
                  onClick={addNoteHere}
                  className="inline-flex h-8 items-center gap-1.5 rounded-(--radius-control) border border-border bg-surface px-2.5 text-xs shadow-(--shadow-button) hover:bg-surface-2"
                >
                  <StickyNote size={14} aria-hidden="true" /> Note
                </button>
              </Panel>
            )}
          </ReactFlow>
        </EdgeDeleter.Provider>
      </NoteSetter.Provider>
      </NodeActionsCtx.Provider>
      {quick && (
        <QuickAdd
          specs={specList}
          x={quick.x}
          y={quick.y}
          noTriggers
          onPick={addFromQuick}
          onClose={() => setQuick(null)}
        />
      )}
    </div>
  );
}

/** Add a node at a flow position and connect it from a port, in one step. */
function addAt(
  d: WfDef,
  spec: NodeSpec,
  at: { x: number; y: number },
  from?: { id: string; port: string },
): { def: WfDef; id: string } {
  return addNode(d, spec, { position: at, ...(from ? { after: from } : {}) });
}

export { addAt };

export function Canvas(props: CanvasProps) {
  return (
    <ReactFlowProvider>
      <Flow {...props} />
    </ReactFlowProvider>
  );
}
