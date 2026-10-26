import { useQuery } from "@tanstack/react-query";
import { Plus, Trash2 } from "lucide-react";
import { type ComponentType, useMemo, useRef, useState } from "react";
import { cn } from "@/lib/cn";
import { aq } from "./api";
import { ExprField } from "./ExprField";
import { CONTROL, JsonField } from "./ParamForm";
import {
  allowedToolNode,
  CODE_MAX,
  freeToolName,
  indentSelection,
  MAX_TOOLS,
  PAGE_MODES,
  PAGE_ROOT,
  PAGE_SAMPLE,
  type PageMode,
  paginationFields,
  paginationProblems,
  pruneForMode,
  returnNodes,
  type ToolDraft,
  toolProblems,
  triggerNodes,
} from "./phase9";
import type { Obj, Schema } from "./types";

// ---------- code.js: a plain code box ----------

const LINE = 20;
const PAD = 8;

/**
 * A code box: monospace, line numbers, Tab inserts spaces (Esc, then Tab, moves on), and the line
 * of an error is marked. It is not an expression field: `${{` in code is just code.
 */
export function CodeField({
  label,
  value,
  onChange,
  errorLine,
  disabled,
  invalid,
}: {
  label: string;
  value: string;
  onChange: (v: string) => void;
  errorLine: number | null;
  disabled?: boolean | undefined;
  invalid?: boolean;
}) {
  const ref = useRef<HTMLTextAreaElement>(null);
  const [scroll, setScroll] = useState({ top: 0, left: 0 });
  const [free, setFree] = useState(false);
  const count = value.split("\n").length;
  const rows = Math.min(24, Math.max(10, count + 1));
  return (
    <div className="flex flex-col gap-1">
      <div
        className={cn(
          "relative flex overflow-hidden rounded-(--radius-control) border bg-(--input-bg) font-mono text-[13px]",
          invalid || errorLine ? "border-danger" : "border-border",
          disabled && "opacity-60",
        )}
        style={{ height: rows * LINE + PAD * 2 }}
      >
        <div
          aria-hidden="true"
          className="w-10 shrink-0 select-none overflow-hidden border-r border-border bg-surface-2 text-right text-muted"
        >
          <div
            style={{
              transform: `translateY(${-scroll.top}px)`,
              paddingTop: PAD,
              paddingBottom: PAD,
            }}
          >
            {Array.from({ length: count }, (_, i) => (
              <div
                key={i}
                className={cn(
                  "pr-2",
                  i + 1 === errorLine && "bg-danger/20 font-semibold text-danger",
                )}
                style={{ height: LINE, lineHeight: `${LINE}px` }}
              >
                {i + 1}
              </div>
            ))}
          </div>
        </div>
        <div className="relative min-w-0 flex-1">
          {errorLine && (
            <div
              aria-hidden="true"
              className="pointer-events-none absolute right-0 left-0 bg-danger/15"
              data-testid="code-error-line"
              style={{ top: PAD + (errorLine - 1) * LINE - scroll.top, height: LINE }}
            />
          )}
          <textarea
            ref={ref}
            aria-label={label}
            value={value}
            disabled={disabled}
            spellCheck={false}
            wrap="off"
            autoCapitalize="off"
            autoCorrect="off"
            maxLength={CODE_MAX}
            onScroll={(e) =>
              setScroll({ top: e.currentTarget.scrollTop, left: e.currentTarget.scrollLeft })
            }
            onChange={(e) => onChange(e.target.value)}
            onKeyDown={(e) => {
              if (e.key === "Escape") {
                setFree(true);
                return;
              }
              if (e.key !== "Tab" || free) {
                if (e.key !== "Shift") setFree(false);
                return;
              }
              e.preventDefault();
              const el = e.currentTarget;
              const r = indentSelection(value, el.selectionStart, el.selectionEnd, e.shiftKey);
              onChange(r.text);
              requestAnimationFrame(() => ref.current?.setSelectionRange(r.start, r.end));
            }}
            onBlur={() => setFree(false)}
            className="absolute inset-0 size-full resize-none overflow-auto border-0 bg-transparent px-3 whitespace-pre text-[13px] outline-none"
            style={{ lineHeight: `${LINE}px`, paddingTop: PAD, paddingBottom: PAD, tabSize: 2 }}
          />
        </div>
      </div>
      <p className="flex justify-between text-xs text-muted">
        <span>Tab indents. Press Esc, then Tab, to leave the box.</span>
        <span className={cn(value.length > CODE_MAX * 0.95 && "text-warning")}>
          {value.length}/{CODE_MAX}
        </span>
      </p>
    </div>
  );
}

// ---------- pickers of workflows and of nodes inside one ----------

/** A picker of the workflows of the user. The workflow being edited is left out (it would call itself). */
export function WorkflowSelect({
  label,
  value,
  onChange,
  disabled,
  excludeId,
  emptyLabel = "Choose…",
}: {
  label: string;
  value: string;
  onChange: (v: string | undefined) => void;
  disabled?: boolean | undefined;
  excludeId?: string;
  emptyLabel?: string;
}) {
  const list = useQuery(aq.workflows({}));
  const rows = (list.data?.workflows ?? []).filter((w) => w.id !== excludeId);
  const known = rows.some((w) => w.id === value);
  return (
    <select
      aria-label={label}
      value={value}
      disabled={disabled}
      onChange={(e) => onChange(e.target.value === "" ? undefined : e.target.value)}
      className={CONTROL}
    >
      <option value="">{emptyLabel}</option>
      {value && !known && <option value={value}>{list.isPending ? "Loading…" : value}</option>}
      {rows.map((w) => (
        <option key={w.id} value={w.id}>
          {w.name}
          {w.published_version === null ? " (not published)" : ""}
        </option>
      ))}
    </select>
  );
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** `trigger` and `return_node` of Execute workflow: a pick from the nodes of the other workflow. */
export function SubNodeField({
  label,
  kind,
  workflowId,
  value,
  onChange,
  disabled,
}: {
  label: string;
  kind: "trigger" | "return_node";
  workflowId: unknown;
  value: string;
  onChange: (v: string | undefined) => void;
  disabled?: boolean | undefined;
}) {
  const id = typeof workflowId === "string" && UUID.test(workflowId) ? workflowId : "";
  const wf = useQuery({ ...aq.workflow(id), enabled: id !== "" });
  const nodes = useMemo(() => {
    const def = wf.data?.draft;
    if (!def) return null;
    return kind === "trigger" ? triggerNodes(def) : returnNodes(def);
  }, [wf.data, kind]);
  const fallback =
    kind === "trigger"
      ? "Default (manual trigger, else the first)"
      : "Default (the last node that made items)";
  if (!nodes) {
    return (
      <input
        aria-label={label}
        value={value}
        disabled={disabled}
        placeholder={id === "" ? "Choose a workflow first, or type a node id" : fallback}
        onChange={(e) => onChange(e.target.value === "" ? undefined : e.target.value)}
        className={cn(CONTROL, "font-mono text-[13px]")}
      />
    );
  }
  const known = nodes.some((n) => n.id === value);
  return (
    <select
      aria-label={label}
      value={value}
      disabled={disabled}
      onChange={(e) => onChange(e.target.value === "" ? undefined : e.target.value)}
      className={CONTROL}
    >
      <option value="">{fallback}</option>
      {value && !known && <option value={value}>{value} (not in that workflow)</option>}
      {nodes.map((n) => (
        <option key={n.id} value={n.id}>
          {n.label === n.id ? n.id : `${n.label} (${n.id})`}
        </option>
      ))}
    </select>
  );
}

// ---------- ai.agent tools ----------

function ErrorLine({ children }: { children: string }) {
  return <p className="text-xs text-danger">{children}</p>;
}

/** The tools an AI agent may call. `Fields` is the field builder of the AI output (for `args`). */
export function ToolsEditor({
  label,
  value,
  onChange,
  disabled,
  Fields,
}: {
  label: string;
  value: ToolDraft[];
  onChange: (v: ToolDraft[] | undefined) => void;
  disabled?: boolean | undefined;
  Fields: ComponentType<{ value: never[]; onChange: (v: never[]) => void }>;
}) {
  const catalog = useQuery(aq.catalog);
  const nodes = (catalog.data ?? []).filter((n) => allowedToolNode(n.type));
  const [dirty, setDirty] = useState<Set<number>>(new Set());
  const problems = toolProblems(value);
  const set = (i: number, patch: Partial<ToolDraft>) => {
    setDirty(new Set(dirty).add(i));
    onChange(value.map((t, j) => (j === i ? { ...t, ...patch } : t)));
  };
  const clean = (v: ToolDraft[]) => onChange(v.length ? v : undefined);
  return (
    <div className="flex flex-col gap-2">
      {value.map((t, i) => {
        const mine = dirty.has(i) ? problems.filter((p) => p.index === i) : [];
        const at = (f: string) => mine.find((p) => p.field === f);
        const node = t.node ?? "";
        return (
          <div
            key={i}
            className="flex flex-col gap-2 rounded-(--radius-control) border border-border p-3"
            data-testid="tool"
          >
            <div className="flex items-center justify-between">
              <span className="text-xs font-medium text-muted">Tool {i + 1}</span>
              <button
                type="button"
                aria-label={`Remove tool ${i + 1}`}
                title="Remove"
                disabled={disabled}
                onClick={() => clean(value.filter((_, j) => j !== i))}
                className="flex size-8 items-center justify-center rounded-(--radius-control) text-muted hover:bg-surface-2 hover:text-text disabled:opacity-50"
              >
                <Trash2 size={15} />
              </button>
            </div>
            <div className="grid gap-2 sm:grid-cols-2">
              <label className="flex flex-col gap-1 text-xs font-medium">
                Name
                <input
                  aria-label={`${label} ${i + 1} name`}
                  value={t.name ?? ""}
                  maxLength={40}
                  disabled={disabled}
                  onChange={(e) => set(i, { name: e.target.value })}
                  className={cn(CONTROL, "font-mono text-[13px]", at("name") && "border-danger")}
                />
                {at("name") && <ErrorLine>{at("name")?.message ?? ""}</ErrorLine>}
              </label>
              <label className="flex flex-col gap-1 text-xs font-medium">
                Node
                <select
                  aria-label={`${label} ${i + 1} node`}
                  value={node}
                  disabled={disabled}
                  onChange={(e) => set(i, { node: e.target.value })}
                  className={cn(CONTROL, at("node") && "border-danger")}
                >
                  <option value="">Choose…</option>
                  {node && !nodes.some((n) => n.type === node) && (
                    <option value={node}>{node}</option>
                  )}
                  {nodes.map((n) => (
                    <option key={n.type} value={n.type}>
                      {n.label} ({n.type})
                    </option>
                  ))}
                </select>
                {at("node") && <ErrorLine>{at("node")?.message ?? ""}</ErrorLine>}
              </label>
            </div>
            <label className="flex flex-col gap-1 text-xs font-medium">
              Description
              <textarea
                aria-label={`${label} ${i + 1} description`}
                value={t.description ?? ""}
                rows={2}
                maxLength={500}
                disabled={disabled}
                placeholder="What the tool does and when to use it. The AI reads this."
                onChange={(e) => set(i, { description: e.target.value })}
                className={cn(
                  "w-full rounded-(--radius-control) border bg-(--input-bg) px-3 py-2 text-sm font-normal",
                  at("description") ? "border-danger" : "border-border",
                )}
              />
              {at("description") && <ErrorLine>{at("description")?.message ?? ""}</ErrorLine>}
            </label>
            <div className="flex flex-col gap-1">
              <span className="text-xs font-medium">Values the AI fills in</span>
              <Fields
                value={(Array.isArray(t.args) ? t.args : []) as never[]}
                onChange={(v) => set(i, { args: v.length ? v : (undefined as never) })}
              />
              <p className="text-xs text-muted">
                The AI sends these as params of the node. It can not send expressions.
              </p>
            </div>
            <details open={t.params !== undefined && Object.keys(t.params).length > 0}>
              <summary className="cursor-pointer text-xs font-medium">Fixed params</summary>
              <div className="mt-1 flex flex-col gap-1">
                <JsonField
                  label={`${label} ${i + 1} fixed params`}
                  value={t.params}
                  disabled={disabled}
                  onChange={(v) =>
                    set(i, {
                      params:
                        v && typeof v === "object" && !Array.isArray(v) ? (v as Obj) : undefined,
                    })
                  }
                />
                <p className="text-xs text-muted">
                  Always win over what the AI sends. Put ids and limits here. {"${{ }}"} works and
                  is read once.
                </p>
              </div>
            </details>
          </div>
        );
      })}
      <button
        type="button"
        disabled={disabled || value.length >= MAX_TOOLS}
        onClick={() => clean([...value, { name: freeToolName(value), description: "", node: "" }])}
        className="inline-flex h-8 w-fit items-center gap-1.5 rounded-(--radius-control) border border-dashed border-border px-2.5 text-xs text-muted hover:bg-surface-2 hover:text-text disabled:opacity-50"
      >
        <Plus size={14} aria-hidden="true" /> Add a tool
      </button>
      <p className="text-xs text-muted">
        {value.length}/{MAX_TOOLS} tools. Do not give a tool that sends or deletes to a prompt that
        reads text from outside.
      </p>
    </div>
  );
}

// ---------- http.request pagination ----------

const MODE_LABEL: Record<PageMode, string> = {
  next_url: "Next address in the answer",
  page: "Page number goes up by 1",
  offset: "Offset goes up by a step",
  cursor: "Cursor from the answer",
};

const PAGE_EXTRA = { roots: PAGE_ROOT, sample: PAGE_SAMPLE };

/** The pagination block of Read an API: off, or a mode with only the fields of that mode. */
export function PaginationEditor({
  schema,
  value,
  onChange,
  nodeId,
  hasSplit,
  disabled,
}: {
  schema: Schema;
  value: unknown;
  onChange: (v: Obj | undefined) => void;
  nodeId: string;
  hasSplit: boolean;
  disabled?: boolean | undefined;
}) {
  const on = value !== undefined && value !== null;
  const pg: Obj = value && typeof value === "object" && !Array.isArray(value) ? (value as Obj) : {};
  const mode = (typeof pg.mode === "string" ? pg.mode : "next_url") as PageMode;
  const props = schema.properties ?? {};
  const set = (k: string, v: unknown) => {
    const next: Obj = { ...pg, mode };
    if (v === undefined || v === "") delete next[k];
    else next[k] = v;
    onChange(next);
  };
  const num = (k: string) => {
    const s = props[k];
    return (
      <input
        aria-label={`Pagination ${k}`}
        type="number"
        value={typeof pg[k] === "number" ? (pg[k] as number) : ""}
        min={s?.minimum}
        max={s?.maximum}
        step={1}
        disabled={disabled}
        placeholder={
          s?.default !== undefined
            ? String(s.default)
            : k === "step" && mode === "offset"
              ? "items on the last page"
              : undefined
        }
        onChange={(e) => set(k, e.target.value === "" ? undefined : Number(e.target.value))}
        className={CONTROL}
      />
    );
  };
  const help: Record<string, string> = {
    next:
      mode === "cursor"
        ? "Where the next cursor is in the answer. Default body.next_cursor."
        : "Where the next address is in the answer, like body.links.next. Default body.next. A relative address works.",
    param:
      mode === "cursor"
        ? "The name of the cursor param, like cursor."
        : mode === "offset"
          ? "The name of the offset param, like offset."
          : "The name of the page param, like page.",
    location: "Where the param goes. Body needs a JSON body.",
    start: mode === "offset" ? "The first offset. Default 0." : "The first page number. Default 1.",
    step: "How much the offset goes up. Default: the items of the last page (needs split on the node).",
    stop_when:
      "True after a page means no more pages. Reads page.number, page.status, page.headers, page.body, page.url, page.items and page.count.",
    max_pages: "Reaching it is not an error. Default 10.",
    delay_ms: "Wait between two pages, in milliseconds.",
  };
  const field = (k: string) => {
    const title =
      k === "stop_when"
        ? "Stop when"
        : k === "max_pages"
          ? "Most pages"
          : k === "delay_ms"
            ? "Wait between pages (ms)"
            : k === "param"
              ? "Param name"
              : k === "next"
                ? "Next path"
                : k.charAt(0).toUpperCase() + k.slice(1);
    let control: React.ReactNode;
    if (k === "location") {
      control = (
        <select
          aria-label="Pagination location"
          value={typeof pg.location === "string" ? pg.location : ""}
          disabled={disabled}
          onChange={(e) => set("location", e.target.value || undefined)}
          className={CONTROL}
        >
          <option value="">Default (query)</option>
          <option value="query">query</option>
          <option value="body">body</option>
        </select>
      );
    } else if (k === "stop_when") {
      control = (
        <ExprField
          label="Pagination stop when"
          value={typeof pg.stop_when === "string" ? pg.stop_when : ""}
          nodeId={nodeId}
          single
          mono
          preview={false}
          extra={PAGE_EXTRA}
          placeholder="${{ page.count == 0 }}"
          disabled={disabled}
          onChange={(v) => set("stop_when", v)}
        />
      );
    } else if (k === "next" || k === "param") {
      control = (
        <input
          aria-label={`Pagination ${k}`}
          value={typeof pg[k] === "string" ? (pg[k] as string) : ""}
          disabled={disabled}
          placeholder={
            k === "next" ? (mode === "cursor" ? "body.next_cursor" : "body.next") : undefined
          }
          onChange={(e) => set(k, e.target.value)}
          className={cn(CONTROL, "font-mono text-[13px]")}
        />
      );
    } else {
      control = num(k);
    }
    return (
      <div key={k} className="flex flex-col gap-1" data-field={k}>
        <span className="text-xs font-medium">
          {title}
          {k === "param" && mode !== "next_url" && <span className="text-danger"> *</span>}
        </span>
        {control}
        <p className="text-xs text-muted">{help[k]}</p>
      </div>
    );
  };
  const problems = on ? paginationProblems(pg, hasSplit) : [];
  return (
    <div className="flex flex-col gap-3 rounded-(--radius-control) border border-border p-3">
      <label className="inline-flex items-center gap-2 text-sm">
        <input
          type="checkbox"
          role="switch"
          aria-label="Read several pages"
          aria-checked={on}
          checked={on}
          disabled={disabled}
          onChange={(e) => onChange(e.target.checked ? { mode: "next_url" } : undefined)}
          className="size-4 accent-(--accent)"
        />
        {on ? "Reads several pages" : "Off: one request"}
      </label>
      {on && (
        <>
          <label className="flex flex-col gap-1 text-xs font-medium">
            How the next page is found
            <select
              aria-label="Pagination mode"
              value={mode}
              disabled={disabled}
              onChange={(e) => onChange(pruneForMode(pg, e.target.value as PageMode))}
              className={CONTROL}
            >
              {PAGE_MODES.map((m) => (
                <option key={m} value={m}>
                  {MODE_LABEL[m]}
                </option>
              ))}
            </select>
          </label>
          {paginationFields(mode).map(field)}
          {problems.map((p) => (
            <p key={p} className="text-xs text-warning">
              {p}
            </p>
          ))}
        </>
      )}
    </div>
  );
}
