import { useQuery } from "@tanstack/react-query";
import { useEffect, useId, useMemo, useRef, useState } from "react";
import { useDebounced } from "@/components/ui";
import { cn } from "@/lib/cn";
import { call } from "./api";
import { useEditor } from "./ctx";
import { type Suggestion, sampleContext, suggest } from "./data";
import type { Obj } from "./types";

interface Props {
  value: string;
  onChange: (v: string) => void;
  /** The node whose param this is. It decides what `json` and `nodes` can read. */
  nodeId: string;
  label: string;
  single?: boolean;
  rows?: number;
  placeholder?: string;
  mono?: boolean;
  /** Show the live value under the field. */
  preview?: boolean;
  invalid?: boolean;
  disabled?: boolean;
  /** Names that only this field may read (`page` in the pagination stop condition), with a sample of their data. */
  extra?: { roots: readonly Suggestion[]; sample: Obj };
}

const SPLIT = /(\$\{\{[\s\S]*?\}\}|\$\{%[\s\S]*?%\}|\$\{#[\s\S]*?#\})/g;

/** Where the caret is inside `${{ }}`: the text of the expression before the caret. */
export function insideExpression(
  value: string,
  caret: number,
): { start: number; before: string } | null {
  const head = value.slice(0, caret);
  const open = head.lastIndexOf("${{");
  if (open < 0) return null;
  if (head.indexOf("}}", open) >= 0) return null;
  return { start: open + 3, before: head.slice(open + 3) };
}

/**
 * A text field for params. `${{ }}` parts are coloured, a list of names pops up inside them,
 * and a line under the field shows the value from the newest test run.
 */
export function ExprField({
  value,
  onChange,
  nodeId,
  label,
  single,
  rows = 1,
  placeholder,
  mono,
  preview = true,
  invalid,
  disabled,
  extra,
}: Props) {
  const ed = useEditor();
  const ref = useRef<HTMLTextAreaElement>(null);
  const listId = useId();
  const [focus, setFocus] = useState(false);
  const [caret, setCaret] = useState(0);
  const [pick, setPick] = useState(0);
  const [forced, setForced] = useState(false);

  const sample = useMemo(
    () =>
      focus
        ? { ...sampleContext(ed.def, ed.specs, ed.dataRun, nodeId), ...(extra?.sample ?? {}) }
        : {},
    [focus, ed.def, ed.specs, ed.dataRun, nodeId, extra],
  );
  const where = focus ? insideExpression(value, caret) : null;
  const sug = where ? suggest(where.before, sample, extra?.roots) : null;
  const items = sug?.items ?? [];
  const show = focus && where !== null && items.length > 0 && (forced || value.length > 0);

  useEffect(() => setPick(0), [items.length, where?.before]);

  function apply(s: Suggestion) {
    if (!where || !sug) return;
    const from = where.start + sug.from;
    const text = s.more ? `${s.text}.` : s.text;
    const next = value.slice(0, from) + text + value.slice(caret);
    // `default('')` and `secret('')`: the caret goes between the quotes
    const at = from + text.length - (text.endsWith("('')") ? 2 : 0);
    onChange(next);
    requestAnimationFrame(() => {
      const el = ref.current;
      if (!el) return;
      const pos = at;
      el.focus();
      el.setSelectionRange(pos, pos);
      setCaret(pos);
    });
  }

  function onKeyDown(e: React.KeyboardEvent<HTMLTextAreaElement>) {
    if (e.key === " " && e.ctrlKey) {
      e.preventDefault();
      setForced(true);
      return;
    }
    if (single && e.key === "Enter" && !show) {
      e.preventDefault();
      return;
    }
    if (!show) return;
    if (e.key === "ArrowDown") {
      e.preventDefault();
      setPick((p) => Math.min(items.length - 1, p + 1));
    } else if (e.key === "ArrowUp") {
      e.preventDefault();
      setPick((p) => Math.max(0, p - 1));
    } else if (e.key === "Enter" || e.key === "Tab") {
      e.preventDefault();
      const s = items[pick];
      if (s) apply(s);
    } else if (e.key === "Escape") {
      e.preventDefault();
      e.stopPropagation();
      setForced(false);
      setFocus(false);
      ref.current?.blur();
    }
  }

  const parts = value.split(SPLIT);
  const text = cn(
    "w-full whitespace-pre-wrap break-words px-3 py-2 text-sm leading-5",
    mono ? "font-mono text-[13px]" : "",
  );

  return (
    <div className="flex flex-col gap-1">
      <div
        className={cn(
          "relative grid rounded-(--radius-control) border bg-(--input-bg)",
          invalid ? "border-danger" : focus ? "border-text/40" : "border-border",
          disabled && "opacity-60",
        )}
      >
        <pre
          aria-hidden="true"
          className={cn(
            text,
            "pointer-events-none col-start-1 row-start-1 m-0 font-[inherit]",
            mono && "font-mono",
          )}
          style={{ minHeight: `${rows * 1.25 + 1}rem` }}
        >
          {parts.map((p, i) =>
            i % 2 === 1 ? (
              <span key={i} className="rounded-sm bg-ai/15 text-ai">
                {p}
              </span>
            ) : (
              <span key={i}>{p}</span>
            ),
          )}
          {"\n"}
        </pre>
        <textarea
          ref={ref}
          aria-label={label}
          aria-autocomplete="list"
          aria-controls={show ? listId : undefined}
          value={value}
          rows={1}
          disabled={disabled}
          spellCheck={false}
          placeholder={placeholder}
          onChange={(e) => {
            onChange(e.target.value);
            setCaret(e.target.selectionStart);
            setForced(false);
          }}
          onKeyDown={onKeyDown}
          onKeyUp={(e) => setCaret(e.currentTarget.selectionStart)}
          onClick={(e) => setCaret(e.currentTarget.selectionStart)}
          onSelect={(e) => setCaret(e.currentTarget.selectionStart)}
          onFocus={(e) => {
            setFocus(true);
            setCaret(e.currentTarget.selectionStart);
          }}
          onBlur={() => {
            // let a click on the list happen first
            setTimeout(() => {
              setFocus(false);
              setForced(false);
            }, 120);
          }}
          className={cn(
            text,
            "col-start-1 row-start-1 h-full resize-none overflow-hidden border-0 bg-transparent font-[inherit] text-transparent caret-(--text) outline-none placeholder:text-muted",
            mono && "font-mono",
          )}
        />
        {show && (
          <div
            id={listId}
            role="listbox"
            aria-label="Suggestions"
            className="absolute top-full left-0 z-50 mt-1 max-h-56 w-72 max-w-full overflow-auto rounded-(--radius-menu) border border-border bg-surface p-1 shadow-(--shadow-pop)"
          >
            {items.slice(0, 40).map((s, i) => (
              <button
                key={s.text}
                type="button"
                role="option"
                aria-selected={i === pick}
                tabIndex={-1}
                onMouseDown={(e) => {
                  e.preventDefault();
                  apply(s);
                }}
                onMouseEnter={() => setPick(i)}
                className={cn(
                  "flex w-full items-baseline justify-between gap-3 rounded-(--radius-control) px-2 py-1 text-left text-sm",
                  i === pick && "bg-surface-2",
                )}
              >
                <span className="font-mono text-[13px]">{s.label ?? s.text}</span>
                {s.hint && <span className="truncate text-xs text-muted">{s.hint}</span>}
              </button>
            ))}
          </div>
        )}
      </div>
      {preview && value.includes("${{") && <Preview text={value} nodeId={nodeId} />}
    </div>
  );
}

function show(v: unknown): string {
  const s = typeof v === "string" ? v : JSON.stringify(v);
  return s.length > 300 ? `${s.slice(0, 300)}…` : s;
}

/** The value of the text with the data of the newest test run (map section 6.6). */
function Preview({ text, nodeId }: { text: string; nodeId: string }) {
  const ed = useEditor();
  const t = useDebounced(text, 300);
  const q = useQuery({
    queryKey: ["automations", "preview", ed.workflowId, nodeId, t, ed.saveTick, ed.dataRun?.id],
    queryFn: () =>
      call.preview({
        workflow_id: ed.workflowId,
        node_id: nodeId,
        text: t,
        ...(ed.dataRun ? { run_id: ed.dataRun.id } : {}),
      }),
    retry: false,
    staleTime: 10_000,
  });
  if (q.isPending) return <p className="text-xs text-muted">…</p>;
  if (q.isError) return null;
  const r = q.data;
  if (r.error) {
    return (
      <p className="text-xs text-danger" role="status">
        {r.error.message}
      </p>
    );
  }
  if (!r.run_id) {
    return <p className="text-xs text-muted">Run a test to see the value here.</p>;
  }
  return (
    <p className="truncate font-mono text-xs text-muted" role="status" title={show(r.value)}>
      <span className="text-text/60">{r.type}</span> {show(r.value)}
    </p>
  );
}
