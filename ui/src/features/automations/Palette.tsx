import { Search } from "lucide-react";
import { forwardRef, useMemo, useState } from "react";
import { cn } from "@/lib/cn";
import { CATEGORY_ICON, CATEGORY_LABEL, CATEGORY_ORDER, iconFor } from "./meta";
import type { Category, NodeSpec } from "./types";

export const DRAG_TYPE = "application/aikido-node";

function matches(s: NodeSpec, q: string): boolean {
  if (!q) return true;
  const hay = `${s.label} ${s.type} ${s.description}`.toLowerCase();
  return q
    .toLowerCase()
    .split(/\s+/)
    .every((w) => hay.includes(w));
}

function groups(specs: NodeSpec[], q: string): [Category, NodeSpec[]][] {
  const out: [Category, NodeSpec[]][] = [];
  for (const c of CATEGORY_ORDER) {
    const list = specs.filter((s) => s.category === c && matches(s, q));
    if (list.length) out.push([c, list]);
  }
  return out;
}

function Item({ spec, onPick, drag }: { spec: NodeSpec; onPick: (s: NodeSpec) => void; drag: boolean }) {
  const Icon = iconFor(spec.type);
  return (
    <button
      type="button"
      draggable={drag}
      onDragStart={(e) => {
        e.dataTransfer.setData(DRAG_TYPE, spec.type);
        e.dataTransfer.effectAllowed = "move";
      }}
      onClick={() => onPick(spec)}
      title={spec.description}
      className="flex w-full items-center gap-2.5 rounded-(--radius-control) px-2 py-1.5 text-left hover:bg-surface-2"
    >
      <span className={cn("flex size-7 shrink-0 items-center justify-center rounded-md", CATEGORY_ICON[spec.category])}>
        <Icon size={15} aria-hidden="true" />
      </span>
      <span className="min-w-0">
        <span className="block truncate text-sm">{spec.label}</span>
        <span className="block truncate text-xs text-muted">{spec.description}</span>
      </span>
    </button>
  );
}

/** The list on the left: search, categories, drag onto the canvas or click to add after the selected node. */
export const Palette = forwardRef<
  HTMLInputElement,
  { specs: NodeSpec[]; onPick: (s: NodeSpec) => void; disabled?: boolean }
>(function Palette({ specs, onPick, disabled }, ref) {
  const [query, setQuery] = useState("");
  const shown = useMemo(() => groups(specs, query.trim()), [specs, query]);
  return (
    <nav aria-label="Nodes" className="flex h-full min-h-0 flex-col border-r border-border bg-surface">
      <div className="relative border-b border-border p-2">
        <Search size={14} aria-hidden="true" className="pointer-events-none absolute top-1/2 left-4.5 -translate-y-1/2 text-muted" />
        <input
          ref={ref}
          aria-label="Search nodes"
          placeholder="Search nodes  ( / )"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          className="h-8 w-full rounded-(--radius-control) border border-border bg-(--input-bg) pr-2 pl-8 text-sm"
        />
      </div>
      <div className={cn("min-h-0 flex-1 overflow-auto p-2", disabled && "pointer-events-none opacity-50")}>
        {shown.length === 0 && <p className="px-2 py-3 text-sm text-muted">No node matches.</p>}
        {shown.map(([cat, list]) => (
          <section key={cat} className="mb-3">
            <h3 className="px-2 pb-1 text-[11px] font-semibold tracking-wide text-muted uppercase">{CATEGORY_LABEL[cat]}</h3>
            {list.map((s) => (
              <Item key={s.type} spec={s} onPick={onPick} drag />
            ))}
          </section>
        ))}
      </div>
    </nav>
  );
});

/** A small search list that opens where you let go of a line in empty space. */
export function QuickAdd({
  specs,
  x,
  y,
  onPick,
  onClose,
  noTriggers,
}: {
  specs: NodeSpec[];
  x: number;
  y: number;
  onPick: (s: NodeSpec) => void;
  onClose: () => void;
  noTriggers: boolean;
}) {
  const [query, setQuery] = useState("");
  const list = useMemo(
    () => groups(specs.filter((s) => !(noTriggers && s.category === "trigger")), query.trim()),
    [specs, query, noTriggers],
  );
  return (
    <div
      role="dialog"
      aria-label="Add a node"
      className="absolute z-50 w-72 rounded-(--radius-menu) border border-border bg-surface shadow-(--shadow-pop)"
      style={{ left: x, top: y }}
      onKeyDown={(e) => {
        if (e.key === "Escape") onClose();
      }}
    >
      <div className="border-b border-border p-2">
        <input
          // biome-ignore lint/a11y/noAutofocus: the popup is opened to type in it
          autoFocus
          aria-label="Search nodes"
          placeholder="Add a node…"
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          className="h-8 w-full rounded-(--radius-control) border border-border bg-(--input-bg) px-2 text-sm"
        />
      </div>
      <div className="max-h-72 overflow-auto p-1.5">
        {list.length === 0 && <p className="px-2 py-2 text-sm text-muted">No node matches.</p>}
        {list.map(([cat, items]) => (
          <section key={cat}>
            <h3 className="px-2 pt-1 pb-0.5 text-[11px] font-semibold tracking-wide text-muted uppercase">{CATEGORY_LABEL[cat]}</h3>
            {items.map((s) => (
              <Item key={s.type} spec={s} onPick={onPick} drag={false} />
            ))}
          </section>
        ))}
      </div>
    </div>
  );
}
