import { ChevronDown, ChevronRight, ChevronLeft } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { cn } from "@/lib/cn";
import type { Obj } from "./types";

type View = "tree" | "table" | "json";

/**
 * Items as a tree, a table or JSON. In the tree every key can be dragged onto a param field:
 * it becomes `${{ prefix.path }}`.
 */
export function ItemsView({
  items,
  more = 0,
  prefix,
  empty = "No data yet.",
}: {
  items: Obj[];
  /** Items the run had but did not send here. */
  more?: number;
  /** `json`, `trigger` or `nodes.x.json`. Without it the keys are not draggable. */
  prefix?: string | undefined;
  empty?: string;
}) {
  const [view, setView] = useState<View>("tree");
  const [index, setIndex] = useState(0);
  if (items.length === 0) return <p className="text-sm text-muted">{empty}</p>;
  const at = Math.min(index, items.length - 1);
  const item = items[at] as Obj;
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between gap-2">
        <div className="inline-flex rounded-(--radius-control) border border-border text-xs" role="group" aria-label="View">
          {(["tree", "table", "json"] as View[]).map((v) => (
            <button
              key={v}
              type="button"
              aria-pressed={view === v}
              onClick={() => setView(v)}
              className={cn("px-2 py-1 capitalize", view === v ? "bg-surface-2 font-medium" : "text-muted")}
            >
              {v === "json" ? "JSON" : v}
            </button>
          ))}
        </div>
        {view !== "table" && items.length > 1 && (
          <div className="flex items-center gap-1 text-xs text-muted">
            <button type="button" aria-label="Previous item" disabled={at === 0} onClick={() => setIndex(at - 1)} className="rounded p-1 hover:bg-surface-2 disabled:opacity-40">
              <ChevronLeft size={14} />
            </button>
            <span>
              Item {at + 1} of {items.length}
              {more > 0 ? ` (+${more} not shown)` : ""}
            </span>
            <button type="button" aria-label="Next item" disabled={at >= items.length - 1} onClick={() => setIndex(at + 1)} className="rounded p-1 hover:bg-surface-2 disabled:opacity-40">
              <ChevronRight size={14} />
            </button>
          </div>
        )}
      </div>
      {view === "tree" && (
        <div className="max-h-96 overflow-auto rounded-(--radius-control) border border-border bg-surface-2/40 p-2 font-mono text-[12px]">
          <Tree value={item} path="" prefix={prefix} depth={0} />
        </div>
      )}
      {view === "json" && (
        <pre className="max-h-96 overflow-auto rounded-(--radius-control) border border-border bg-surface-2/40 p-2 font-mono text-[12px] whitespace-pre-wrap break-words">
          {JSON.stringify(items.length === 1 ? item : items, null, 2)}
        </pre>
      )}
      {view === "table" && <Table items={items} more={more} />}
    </div>
  );
}

function expressionFor(prefix: string, path: string): string {
  return `\${{ ${prefix}${path ? `.${path}` : ""} }}`;
}

function Leaf({ value }: { value: unknown }) {
  if (value === null) return <span className="text-muted">null</span>;
  if (typeof value === "string")
    return <span className="text-success break-all">"{value.length > 200 ? `${value.slice(0, 200)}…` : value}"</span>;
  if (typeof value === "number") return <span className="text-info">{value}</span>;
  return <span className="text-warning">{String(value)}</span>;
}

function Tree({
  value,
  path,
  prefix,
  depth,
}: {
  value: unknown;
  path: string;
  prefix: string | undefined;
  depth: number;
}) {
  if (value === null || typeof value !== "object") return <Leaf value={value} />;
  const entries: [string, unknown][] = Array.isArray(value)
    ? value.map((v, i) => [String(i), v])
    : Object.entries(value as Obj);
  if (entries.length === 0) return <span className="text-muted">{Array.isArray(value) ? "[]" : "{}"}</span>;
  return (
    <ul className={cn("flex flex-col gap-0.5", depth > 0 && "ml-3 border-l border-border pl-2")}>
      {entries.map(([k, v]) => (
        <Row key={k} name={k} value={v} path={path ? `${path}.${k}` : k} prefix={prefix} depth={depth} />
      ))}
    </ul>
  );
}

function Row({
  name,
  value,
  path,
  prefix,
  depth,
}: {
  name: string;
  value: unknown;
  path: string;
  prefix: string | undefined;
  depth: number;
}) {
  const [open, setOpen] = useState(depth < 1);
  const branch = value !== null && typeof value === "object";
  const expr = prefix ? expressionFor(prefix, path) : null;
  const size = branch ? (Array.isArray(value) ? value.length : Object.keys(value as Obj).length) : 0;
  return (
    <li>
      <div className="flex items-baseline gap-1">
        {branch ? (
          <button type="button" aria-label={open ? "Collapse" : "Expand"} onClick={() => setOpen(!open)} className="shrink-0 self-center text-muted">
            {open ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
          </button>
        ) : (
          <span className="w-3 shrink-0" />
        )}
        <button
          type="button"
          draggable={expr !== null}
          onDragStart={(e) => {
            if (!expr) return;
            e.dataTransfer.setData("text/plain", expr);
            e.dataTransfer.effectAllowed = "copy";
          }}
          onClick={async () => {
            if (!expr) return;
            try {
              await navigator.clipboard.writeText(expr);
              toast.success("Copied. Paste it into a field.");
            } catch {
              // the browser refused; the key can still be dragged
            }
          }}
          title={expr ? `Drag into a field: ${expr}` : undefined}
          className={cn("shrink-0 rounded px-1 text-left text-text", expr && "cursor-grab hover:bg-accent-soft")}
        >
          {name}
        </button>
        {branch ? (
          <span className="text-muted">{Array.isArray(value) ? `[${size}]` : `{${size}}`}</span>
        ) : (
          <Leaf value={value} />
        )}
      </div>
      {branch && open && <Tree value={value} path={path} prefix={prefix} depth={depth + 1} />}
    </li>
  );
}

function cell(v: unknown): string {
  if (v === null || v === undefined) return "";
  const s = typeof v === "string" ? v : JSON.stringify(v);
  return s.length > 80 ? `${s.slice(0, 80)}…` : s;
}

function Table({ items, more }: { items: Obj[]; more: number }) {
  const cols = Array.from(new Set(items.flatMap((i) => Object.keys(i)))).slice(0, 12);
  return (
    <div className="max-h-96 overflow-auto rounded-(--radius-control) border border-border">
      <table className="w-full border-collapse text-left text-xs">
        <thead className="sticky top-0 bg-surface">
          <tr>
            <th className="px-2 py-1.5 text-muted">#</th>
            {cols.map((c) => (
              <th key={c} className="px-2 py-1.5 font-medium">
                {c}
              </th>
            ))}
          </tr>
        </thead>
        <tbody>
          {items.map((it, i) => (
            <tr key={i} className="border-t border-border">
              <td className="px-2 py-1 text-muted">{i + 1}</td>
              {cols.map((c) => (
                <td key={c} className="max-w-48 truncate px-2 py-1 font-mono">
                  {cell(it[c])}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
      {more > 0 && <p className="border-t border-border px-2 py-1 text-xs text-muted">+{more} more items are not shown.</p>}
    </div>
  );
}
