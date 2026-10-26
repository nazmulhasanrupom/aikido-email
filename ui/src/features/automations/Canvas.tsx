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
  useReactFlow,
} from "@xyflow/react";
import "@xyflow/react/dist/base.css";
import { AlignHorizontalSpaceAround, StickyNote, X } from "lucide-react";
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

const WfNodeCard = memo(function WfNodeCard({ id, selected }: NodeProps) {
  const ed = useEditor();
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
        "relative w-55 border bg-surface text-left shadow-(--shadow-card)",
        trigger ? "rounded-r-[10px] rounded-l-sm" : "rounded-[10px]",
        selected ? "border-accent" : "border-border",
        node.disabled && "opacity-50",
        ed.flash.includes(id) && "wf-flash",
        row && RING[row.status],
      )}
      style={{ minHeight: Math.max(NODE_H - 8, ports.length * 22 + 14) }}
    >
      {!trigger && <Handle type="target" position={Position.Left} id="in" className="wf-handle" />}
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
  return (
    <>
      <BaseEdge
        id={id}
        path={path}
        {...(markerEnd ? { markerEnd } : {})}
        className={d?.live ? "wf-edge-live" : ""}
        interactionWidth={24}
      />
      <EdgeLabelRenderer>
        <div
          className="nodrag nopan absolute flex items-center gap-1"
          style={{
            transform: `translate(-50%, -50%) translate(${lx}px, ${ly}px)`,
            pointerEvents: "all",
          }}
        >
          {d?.count !== undefined && (
            <span className="rounded-full border border-border bg-surface px-1.5 text-[10px] leading-4 text-muted">
              {d.count} {d.count === 1 ? "item" : "items"}
            </span>
          )}
          {selected && !ed.readOnly && d && (
            <button
              type="button"
              aria-label="Delete the line"
              onClick={() => del({ from: d.from, port: d.port, to: d.to })}
              className="flex size-5 items-center justify-center rounded-full border border-border bg-surface text-muted hover:text-danger"
            >
              <X size={12} />
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

function Flow({ update, mark, selected, onSelect, onAdded, onPasteWorkflow }: CanvasProps) {
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

  // copy and paste (the browser's own events, so they work with the system clipboard)
  useEffect(() => {
    if (ed.readOnly) return;
    const onCopy = (e: ClipboardEvent) => {
      if (isTyping(document.activeElement) || window.getSelection()?.toString()) return;
      const ids = selected.filter((s) => !s.startsWith(NOTE_PREFIX));
      if (ids.length === 0) return;
      e.clipboardData?.setData("text/plain", JSON.stringify(fragmentOf(ed.def, ids), null, 2));
      e.preventDefault();
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
    document.addEventListener("paste", onPaste);
    return () => {
      document.removeEventListener("copy", onCopy);
      document.removeEventListener("paste", onPaste);
    };
  }, [ed.def, ed.readOnly, selected, update, onSelect, onPasteWorkflow]);

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
            onNodesDelete={(del) => {
              const ids = del.map((n) => n.id);
              const real = ids.filter((i) => !i.startsWith(NOTE_PREFIX));
              const notes = ids
                .filter((i) => i.startsWith(NOTE_PREFIX))
                .map((i) => i.slice(NOTE_PREFIX.length));
              update((d) => {
                let next = real.length ? removeNodes(d, real) : d;
                if (notes.length)
                  next = {
                    ...next,
                    notes: (next.notes ?? []).filter((n) => !notes.includes(n.id)),
                  };
                return next;
              });
            }}
            onEdgesDelete={(del) => {
              const gone = new Set(del.map((e) => e.id));
              update((d) => ({
                ...d,
                edges: d.edges.filter((e: WfEdge) => !gone.has(`${e.from}|${portOf(e)}|${e.to}`)),
              }));
            }}
            onPaneClick={() => setQuick(null)}
            nodesDraggable={!ed.readOnly}
            nodesConnectable={!ed.readOnly}
            elementsSelectable
            deleteKeyCode={ed.readOnly ? null : DELETE_KEYS}
            multiSelectionKeyCode={MULTI_KEYS}
            selectionOnDrag={false}
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
