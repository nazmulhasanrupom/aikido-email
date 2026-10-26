import { useQuery } from "@tanstack/react-query";
import { Copy, FlaskConical, Pin, PinOff, Trash2, X } from "lucide-react";
import { useState } from "react";
import { q } from "@/api/queries";
import { Button, Modal, Notice } from "@/components/ui";
import { cn } from "@/lib/cn";
import { AikidoApiForm } from "./AikidoApiForm";
import { useEditor } from "./ctx";
import { dataOf } from "./data";
import { ItemsView } from "./DataView";
import { ID_RE, isTrigger, outPorts, parentOf, portLabel, RESERVED, upstream } from "./defs";
import { CATEGORY_ICON, iconFor } from "./meta";
import { CONTROL, JsonField, ObjectForm, ORDER } from "./ParamForm";
import type { TestInput } from "./testInput";
import { CronPreview, type Listening, TriggerTools } from "./TriggerTools";
import type { NodeSpec, Obj, OnError, WfNode } from "./types";

type Tab = "params" | "input" | "output";

export interface PanelProps {
  node: WfNode;
  spec: NodeSpec | undefined;
  onParams: (p: Obj) => void;
  onPatch: (patch: Partial<WfNode>) => void;
  onRename: (id: string) => void;
  onDelete: () => void;
  onDuplicate: () => void;
  onClose: () => void;
  onTestStep: () => void;
  onPin: (items: Obj[] | null) => void;
  testInput: TestInput | undefined;
  onTestInput: (patch: TestInput) => void;
  listening: Listening | null;
  onListen: () => void;
  busy: boolean;
}

/** The right side of the editor: parameters, input and output of the selected node (map 17.5). */
export function NodePanel(p: PanelProps) {
  const ed = useEditor();
  const [tab, setTab] = useState<Tab>("params");
  const { node, spec } = p;
  const Icon = iconFor(node.type);
  const mine = ed.issues.get(node.id) ?? [];
  const out = dataOf(ed.def, ed.specs, ed.dataRun, node.id, false);
  const outCount = Object.values(out.ports).reduce((n, l) => n + l.length, 0);
  return (
    <aside aria-label="Node" className="flex h-full min-h-0 w-full flex-col border-l border-border bg-surface">
      <header className="flex items-start gap-2 border-b border-border p-3">
        <span className={cn("flex size-8 shrink-0 items-center justify-center rounded-lg", CATEGORY_ICON[spec?.category ?? "flow"])}>
          <Icon size={16} aria-hidden="true" />
        </span>
        <div className="min-w-0 flex-1">
          <input
            aria-label="Node name"
            value={node.name ?? ""}
            placeholder={spec?.label}
            disabled={ed.readOnly}
            onChange={(e) => p.onPatch({ name: e.target.value })}
            className="w-full rounded bg-transparent px-1 py-0.5 text-sm font-semibold hover:bg-surface-2 focus:bg-surface-2"
          />
          <p className="truncate px-1 text-xs text-muted">{spec?.label ?? node.type}</p>
        </div>
        {!ed.readOnly && (
          <>
            <IconBtn label="Duplicate node" onClick={p.onDuplicate}>
              <Copy size={15} />
            </IconBtn>
            <IconBtn label="Delete node" onClick={p.onDelete}>
              <Trash2 size={15} />
            </IconBtn>
          </>
        )}
        <IconBtn label="Close" onClick={p.onClose}>
          <X size={16} />
        </IconBtn>
      </header>
      <div role="tablist" aria-label="Node tabs" className="flex gap-1 border-b border-border px-2">
        {(
          [
            ["params", "Parameters"],
            ["input", "Input"],
            ["output", outCount > 0 ? `Output (${outCount})` : "Output"],
          ] as [Tab, string][]
        ).map(([v, l]) => (
          <button
            key={v}
            type="button"
            role="tab"
            aria-selected={tab === v}
            onClick={() => setTab(v)}
            className={cn("-mb-px border-b-2 px-2.5 py-2 text-sm", tab === v ? "border-(--tab-indicator) font-medium" : "border-transparent text-muted hover:text-text")}
          >
            {l}
          </button>
        ))}
      </div>
      <div className="min-h-0 flex-1 overflow-auto p-3">
        {tab === "params" && <ParamsTab {...p} />}
        {tab === "input" && <InputTab node={node} />}
        {tab === "output" && <OutputTab {...p} />}
        {mine.some((i) => !i.path) && (
          <ul className="mt-4 flex flex-col gap-1">
            {mine
              .filter((i) => !i.path)
              .map((i, n) => (
                <li key={n} className={cn("text-xs", i.level === "error" ? "text-danger" : "text-warning")}>
                  {i.message}
                </li>
              ))}
          </ul>
        )}
      </div>
    </aside>
  );
}

function IconBtn({ label, onClick, children }: { label: string; onClick: () => void; children: React.ReactNode }) {
  return (
    <button type="button" aria-label={label} title={label} onClick={onClick} className="flex size-8 shrink-0 items-center justify-center rounded-(--radius-control) text-muted hover:bg-surface-2 hover:text-text">
      {children}
    </button>
  );
}

// ---------- parameters ----------

function ParamsTab(p: PanelProps) {
  const ed = useEditor();
  const { node, spec } = p;
  const providers = useQuery({ ...q.providers, enabled: node.type === "ai.agent" });
  const issues = ed.issues.get(node.id) ?? [];
  const params = node.params ?? {};
  if (!spec) {
    return <Notice tone="danger">The node type {node.type} is not known on this server.</Notice>;
  }
  return (
    <div className="flex flex-col gap-4">
      <p className="text-sm text-muted">{spec.description}</p>
      {spec.admin_only && <Notice tone="warning">Only an owner or admin can publish a workflow with this node.</Notice>}
      {node.type === "ai.agent" && providers.data && providers.data.length === 0 && (
        <Notice tone="warning">Set up an AI provider in Settings &gt; AI to run this node.</Notice>
      )}
      {node.type === "http.request" && String(params.method ?? "GET") !== "GET" && (
        <p className="text-xs text-warning">A test run really sends this {String(params.method)} request.</p>
      )}
      {isTrigger(node.type) && (
        <TriggerTools node={node} input={p.testInput} onInput={p.onTestInput} listening={p.listening} onListen={p.onListen} readOnly={ed.readOnly} />
      )}
      {node.type === "aikido.api" ? (
        <AikidoApiForm nodeId={node.id} params={params} onChange={p.onParams} issues={issues} {...(ed.readOnly ? { disabled: true } : {})} />
      ) : (
        <ObjectForm
          schema={spec.params_schema}
          value={params}
          path="params"
          ctx={{ nodeId: node.id, issues, ...(ed.readOnly ? { disabled: true } : {}) }}
          onChange={p.onParams}
          {...(ORDER[node.type] ? { order: ORDER[node.type] } : {})}
        />
      )}
      {node.type === "trigger.cron" && typeof params.expression === "string" && (
        <CronPreview expression={params.expression} timezone={String(params.timezone || ed.def.settings?.timezone || "UTC")} />
      )}
      <NodeSettings {...p} />
    </div>
  );
}

function NodeSettings({ node, spec, onPatch, onRename }: PanelProps) {
  const ed = useEditor();
  const [open, setOpen] = useState(false);
  const [id, setId] = useState(node.id);
  const taken = ed.def.nodes.some((n) => n.id !== node.id && n.id === id);
  const badId = id !== node.id && (!ID_RE.test(id) || RESERVED.has(id) || taken);
  const trig = isTrigger(node.type);
  const dis = ed.readOnly;
  return (
    <section className="rounded-(--radius-control) border border-border">
      <button type="button" aria-expanded={open} onClick={() => setOpen(!open)} className="flex w-full items-center justify-between px-3 py-2 text-left text-sm font-medium">
        Settings
        <span className="text-xs text-muted">{open ? "Hide" : "On error, retry, batch, notes, id"}</span>
      </button>
      {open && (
        <div className="flex flex-col gap-3 border-t border-border p-3">
          <label className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Node id</span>
            <input
              value={id}
              disabled={dis}
              onChange={(e) => setId(e.target.value)}
              onBlur={() => {
                if (!badId && id !== node.id) onRename(id);
                else setId(node.id);
              }}
              className={cn(CONTROL, "font-mono text-[13px]", badId && "border-danger")}
            />
            <span className="text-xs text-muted">Other nodes read this one as nodes.{node.id}. Renaming updates them.</span>
            {badId && <span className="text-xs text-danger">{taken ? "Another node has this id." : "Use a-z, 0-9 and _, start with a letter, and not a reserved word."}</span>}
          </label>
          <label className="inline-flex items-center gap-2 text-sm">
            <input type="checkbox" checked={!!node.disabled} disabled={dis} onChange={(e) => onPatch({ disabled: e.target.checked || (undefined as never) })} className="accent-(--accent)" />
            Disabled (items pass through unchanged)
          </label>
          {!trig && (
            <>
              <label className="flex flex-col gap-1 text-sm">
                <span className="font-medium">On error</span>
                <select
                  value={node.on_error ?? ""}
                  disabled={dis}
                  onChange={(e) => onPatch({ on_error: (e.target.value || undefined) as OnError })}
                  className={CONTROL}
                >
                  <option value="">Workflow default</option>
                  <option value="stop">Stop the run</option>
                  <option value="continue">Continue with an error field</option>
                  <option value="error_port">Send to the error port</option>
                </select>
              </label>
              <div className="grid grid-cols-2 gap-2">
                <NumberBox label="Retries" value={node.retry?.max ?? 0} min={0} max={5} disabled={dis} onChange={(n) => onPatch({ retry: n > 0 ? { max: n, delay_ms: node.retry?.delay_ms ?? 1000 } : (undefined as never) })} />
                <NumberBox label="Retry wait (ms)" value={node.retry?.delay_ms ?? 1000} min={100} max={60000} disabled={dis || !node.retry} onChange={(n) => onPatch({ retry: { max: node.retry?.max ?? 1, delay_ms: n } })} />
              </div>
              {spec?.mode === "per_item" && (
                <div className="grid grid-cols-2 gap-2">
                  <NumberBox label="Batch size" value={node.batch?.size ?? 1} min={1} max={100} disabled={dis} onChange={(n) => onPatch({ batch: n > 1 || node.batch?.delay_ms ? { size: n, delay_ms: node.batch?.delay_ms ?? 0 } : (undefined as never) })} />
                  <NumberBox label="Wait between (ms)" value={node.batch?.delay_ms ?? 0} min={0} max={600000} disabled={dis} onChange={(n) => onPatch({ batch: n > 0 || (node.batch?.size ?? 1) > 1 ? { size: node.batch?.size ?? 1, delay_ms: n } : (undefined as never) })} />
                </div>
              )}
            </>
          )}
          <label className="flex flex-col gap-1 text-sm">
            <span className="font-medium">Notes</span>
            <textarea
              value={node.notes ?? ""}
              disabled={dis}
              maxLength={500}
              rows={2}
              onChange={(e) => onPatch({ notes: e.target.value || (undefined as never) })}
              className="w-full rounded-(--radius-control) border border-border bg-(--input-bg) px-3 py-2 text-sm"
            />
          </label>
        </div>
      )}
    </section>
  );
}

function NumberBox({ label, value, min, max, onChange, disabled }: { label: string; value: number; min: number; max: number; onChange: (n: number) => void; disabled?: boolean }) {
  return (
    <label className="flex flex-col gap-1 text-sm">
      <span className="font-medium">{label}</span>
      <input type="number" value={value} min={min} max={max} disabled={disabled} onChange={(e) => onChange(Math.max(min, Math.min(max, Number(e.target.value) || 0)))} className={CONTROL} />
    </label>
  );
}

// ---------- input ----------

function InputTab({ node }: { node: WfNode }) {
  const ed = useEditor();
  const ups = upstream(ed.def, node.id);
  const parent = parentOf(ed.def, node.id)?.from;
  if (isTrigger(node.type)) {
    return <p className="text-sm text-muted">A trigger has no input. Its data is on the Output tab.</p>;
  }
  if (ups.length === 0) {
    return <p className="text-sm text-muted">Connect this node to another one to see the data it can read.</p>;
  }
  // the trigger first, then the others, nearest first
  const trigger = ups.filter((u) => isTrigger(ed.def.nodes.find((n) => n.id === u)?.type ?? ""));
  const rest = ups.filter((u) => !trigger.includes(u));
  const order = [...rest, ...trigger];
  return (
    <div className="flex flex-col gap-3">
      <p className="text-xs text-muted">Drag a key into a field to use it.</p>
      {order.map((id, n) => {
        const isTrig = trigger.includes(id);
        const prefix = isTrig ? "trigger" : id === parent ? "json" : `nodes.${id}.json`;
        const d = dataOf(ed.def, ed.specs, ed.dataRun, id);
        const items = Object.values(d.ports).flat();
        const name = ed.def.nodes.find((x) => x.id === id)?.name || id;
        return (
          <details key={id} open={n === 0} className="rounded-(--radius-control) border border-border">
            <summary className="flex cursor-pointer items-center justify-between gap-2 px-3 py-2 text-sm font-medium">
              <span className="truncate">{name}</span>
              <span className="shrink-0 text-xs font-normal text-muted">{d.source === "pin" ? "pinned" : d.source === "example" ? "example" : d.source === "run" ? `${items.length} item${items.length === 1 ? "" : "s"}` : "no data"}</span>
            </summary>
            <div className="border-t border-border p-3">
              {d.source === "example" && <p className="mb-2 text-xs text-muted">No test run yet. This is an example.</p>}
              <ItemsView items={items} more={Object.values(d.more).reduce((a, b) => a + b, 0)} prefix={prefix} empty="This node has not given data yet." />
              {id !== parent && !isTrig && <p className="mt-2 text-xs text-muted">Read as {`\${{ nodes.${id}.json.<key> }}`}.</p>}
            </div>
          </details>
        );
      })}
    </div>
  );
}

// ---------- output ----------

function OutputTab(p: PanelProps) {
  const ed = useEditor();
  const { node } = p;
  const d = dataOf(ed.def, ed.specs, ed.dataRun, node.id, false);
  const ports = Object.keys(d.ports);
  const [port, setPort] = useState<string | null>(null);
  const [editing, setEditing] = useState(false);
  const shown = port && ports.includes(port) ? port : (ports[0] ?? "main");
  const pinned = !!ed.def.pins?.[node.id]?.length;
  const row = ed.dataRun?.nodes.filter((n) => n.node_id === node.id).at(-1);
  const portNames = outPorts(node, p.spec);
  return (
    <div className="flex flex-col gap-3">
      <div className="flex flex-wrap items-center gap-2">
        {!ed.readOnly && (
          <Button variant="secondary" className="h-8" busy={p.busy} onClick={p.onTestStep}>
            <FlaskConical size={14} aria-hidden="true" /> Test step
          </Button>
        )}
        {!ed.readOnly && d.source === "run" && (
          <Button variant="secondary" className="h-8" onClick={() => p.onPin(d.ports.main ?? Object.values(d.ports).flat())}>
            <Pin size={14} aria-hidden="true" /> Pin output
          </Button>
        )}
        {!ed.readOnly && pinned && (
          <>
            <Button variant="secondary" className="h-8" onClick={() => setEditing(true)}>
              Edit pin
            </Button>
            <Button variant="secondary" className="h-8" onClick={() => p.onPin(null)}>
              <PinOff size={14} aria-hidden="true" /> Unpin
            </Button>
          </>
        )}
        {!ed.readOnly && !pinned && <Button variant="ghost" className="h-8" onClick={() => setEditing(true)}>Write a pin</Button>}
      </div>
      {pinned && <p className="text-xs text-muted">Pinned: test runs use this data and do not call the node. Production runs ignore pins.</p>}
      {row?.status === "dry" && <Notice tone="info">Dry run: this node did not send anything. The data shows the call it would make.</Notice>}
      {row?.error && (
        <Notice tone="danger">
          <strong>{row.error.code}</strong>: {row.error.message}
        </Notice>
      )}
      {ports.length > 1 && (
        <div role="tablist" aria-label="Ports" className="flex gap-1">
          {ports.map((pt) => (
            <button key={pt} type="button" role="tab" aria-selected={shown === pt} onClick={() => setPort(pt)} className={cn("rounded-full border px-2.5 py-0.5 text-xs", shown === pt ? "border-text/40 bg-surface-2 font-medium" : "border-border text-muted")}>
              {portLabel(pt) || "main"} ({d.ports[pt]?.length ?? 0})
            </button>
          ))}
        </div>
      )}
      {portNames.length > 1 && ports.length <= 1 && d.source !== "none" && <p className="text-xs text-muted">Ports: {portNames.map((x) => portLabel(x) || "main").join(", ")}.</p>}
      <ItemsView items={d.ports[shown] ?? []} more={d.more[shown] ?? 0} prefix={`nodes.${node.id}.json`} empty={row?.status === "skipped" ? "This node was skipped: it got no items." : "No data yet. Test the workflow or press Test step."} />
      {editing && <PinEditor open onClose={() => setEditing(false)} current={ed.def.pins?.[node.id] ?? d.ports.main ?? []} onSave={(items) => p.onPin(items)} />}
    </div>
  );
}

function PinEditor({ open, onClose, current, onSave }: { open: boolean; onClose: () => void; current: Obj[]; onSave: (items: Obj[]) => void }) {
  const [value, setValue] = useState<unknown>(current);
  const [error, setError] = useState<string | null>(null);
  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Pin output"
      size="lg"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button
            onClick={() => {
              const list = Array.isArray(value) ? value : value && typeof value === "object" ? [value] : null;
              if (!list || list.length === 0 || !list.every((x) => x && typeof x === "object" && !Array.isArray(x))) {
                setError("Give a JSON object, or a list of objects.");
                return;
              }
              onSave(list as Obj[]);
              onClose();
            }}
          >
            Save pin
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-2">
        <p className="text-sm text-muted">The items this node gives in test runs. Each item is a JSON object.</p>
        <JsonField label="Pinned items" value={value} onChange={(v) => { setValue(v); setError(null); }} rows={14} />
        {error && <p className="text-xs text-danger">{error}</p>}
      </div>
    </Modal>
  );
}
