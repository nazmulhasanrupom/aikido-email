import { useQuery } from "@tanstack/react-query";
import { Plus, Trash2 } from "lucide-react";
import { type ReactNode, useEffect, useMemo, useState } from "react";
import { q } from "@/api/queries";
import { cn } from "@/lib/cn";
import { aq } from "./api";
import { type Leveled, useEditor } from "./ctx";
import { isExpression } from "./defs";
import { ExprField } from "./ExprField";
import {
  CodeField,
  PaginationEditor,
  SubNodeField,
  ToolsEditor,
  WorkflowSelect,
} from "./Phase9Widgets";
import { codeErrorLine, type ToolDraft } from "./phase9";
import type { Obj, Schema } from "./types";

export const CONTROL =
  "h-9 w-full rounded-(--radius-control) border border-border bg-(--input-bg) px-3 text-sm placeholder:text-muted disabled:opacity-60";

export function humanize(key: string): string {
  const t = key.replace(/_/g, " ");
  return t.charAt(0).toUpperCase() + t.slice(1);
}

interface Ctx {
  nodeId: string;
  /** The issues of this node. */
  issues: Leveled[];
  disabled?: boolean;
}

function issuesAt(issues: Leveled[], path: string): Leveled[] {
  return issues.filter(
    (i) => i.path === path || i.path?.startsWith(`${path}.`) || i.path?.startsWith(`${path}[`),
  );
}

function Issues({ list }: { list: Leveled[] }) {
  if (list.length === 0) return null;
  return (
    <ul className="flex flex-col gap-0.5">
      {list.map((i, n) => (
        <li key={n} className={cn("text-xs", i.level === "error" ? "text-danger" : "text-warning")}>
          {i.message}
        </li>
      ))}
    </ul>
  );
}

// ---------- the form ----------

/** The order of the fields of nodes where the schema's own (alphabetical) order reads badly. */
export const ORDER: Record<string, string[]> = {
  "ai.agent": [
    "provider",
    "system",
    "prompt",
    "output",
    "tools",
    "max_tool_calls",
    "temperature",
    "max_tokens",
    "timeout_secs",
    "strip_dashes",
  ],
  "flow.execute_workflow": ["workflow_id", "input", "trigger", "return_node"],
  "http.request": [
    "method",
    "url",
    "query",
    "headers",
    "body_type",
    "body",
    "auth",
    "response_format",
    "split",
    "pagination",
    "fail_on_status",
    "follow_redirects",
    "timeout_secs",
  ],
  "web.fetch_page": ["url", "extract", "max_chars", "include_links", "respect_robots"],
  "trigger.webhook": ["path", "method", "auth", "response"],
  "trigger.schedule": ["mode", "every", "unit", "time", "days", "catch_up"],
  "flow.wait": ["mode", "amount", "unit", "until"],
  "data.set": ["fields", "keep"],
  "flow.if": ["conditions", "combine", "expression"],
  "flow.filter": ["conditions", "combine", "expression"],
};

const FIRST = /^(mode|format|type|method|action|kind|operation)$/;
const LAST = /^(timeout|max_|retry|strip|limit|delay|catch_up|follow|fail_on|respect|include)/;

/** Required fields first, then the ones that choose how the rest works, then the rest; "advanced" ones last. */
export function orderedProps(schema: Schema, explicit?: string[]): [string, Schema][] {
  const props = schema.properties ?? {};
  const keys = Object.keys(props);
  const required = schema.required ?? [];
  const rank = (k: string) => (required.includes(k) ? 0 : FIRST.test(k) ? 1 : LAST.test(k) ? 3 : 2);
  const sorted = explicit
    ? [...explicit.filter((k) => k in props), ...keys.filter((k) => !explicit.includes(k))]
    : [...keys].sort(
        (a, b) =>
          rank(a) - rank(b) ||
          (rank(a) === 0 ? required.indexOf(a) - required.indexOf(b) : a.localeCompare(b)),
      );
  return sorted.map((k) => [k, props[k] as Schema]);
}

/** Fields that only matter in one mode are hidden in the others. */
const MODES: Record<string, string[]> = {
  every: ["interval"],
  time: ["daily"],
  days: ["daily"],
  amount: ["duration"],
  until: ["until"],
  unit: ["interval", "duration"],
};

function hidden(key: string, schema: Schema, value: Obj): boolean {
  const props = schema.properties ?? {};
  if (key === "fields" && "format" in props)
    return value.format !== undefined && value.format !== "json";
  const modes = MODES[key];
  if (modes && "mode" in props && typeof value.mode === "string")
    return !modes.includes(value.mode);
  return false;
}

/** The fields of an object schema, one after the other. */
export function ObjectForm({
  schema,
  value,
  onChange,
  path,
  ctx,
  order,
}: {
  schema: Schema;
  value: Obj;
  onChange: (v: Obj) => void;
  path: string;
  ctx: Ctx;
  order?: string[];
}) {
  const required = new Set(schema.required ?? []);
  const props = orderedProps(schema, order).filter(([key]) => !hidden(key, schema, value));
  if (props.length === 0) {
    return <p className="text-sm text-muted">This node has no settings.</p>;
  }
  return (
    <div className="flex flex-col gap-4">
      {props.map(([key, sub]) => (
        <Prop
          key={key}
          name={key}
          schema={sub}
          required={required.has(key)}
          value={value[key]}
          path={`${path}.${key}`}
          ctx={ctx}
          parent={schema}
          siblings={value}
          onChange={(v) => {
            const next = { ...value };
            if (v === undefined || (typeof v === "string" && v === "" && !required.has(key)))
              delete next[key];
            else next[key] = v;
            onChange(next);
          }}
        />
      ))}
    </div>
  );
}

type Kind =
  | "rules"
  | "fields"
  | "keyvalue"
  | "enum"
  | "source"
  | "text"
  | "longtext"
  | "number"
  | "boolean"
  | "multienum"
  | "stringlist"
  | "objectlist"
  | "group"
  | "code"
  | "pagination"
  | "tools"
  | "subnode"
  | "json";

/** Params with a form of their own (Phase 9): they are known by their name and shape. */
export function specialKind(name: string, s: Schema, parent?: Schema): Kind | null {
  if (name === "pagination" && s.type === "object" && s.properties?.mode && s.properties.max_pages)
    return "pagination";
  if (
    name === "tools" &&
    s.type === "array" &&
    s.items?.properties?.node &&
    s.items.properties.args
  )
    return "tools";
  if (
    (name === "trigger" || name === "return_node") &&
    s.type === "string" &&
    parent?.properties?.workflow_id?.["x-ui"]?.source === "workflows"
  )
    return "subnode";
  if (s.type === "string" && s["x-ui"]?.widget === "code") return "code";
  return null;
}

function kindOf(s: Schema): Kind {
  const w = s["x-ui"]?.widget;
  if (w === "rules") return "rules";
  if (w === "fields") return "fields";
  if (w === "keyvalue") return "keyvalue";
  if (s.enum && s.type === "string") return "enum";
  if (s["x-ui"]?.source && s.type === "string") return "source";
  switch (s.type) {
    case "string":
      return w === "textarea" || w === "code" || (s.maxLength ?? 0) > 400 ? "longtext" : "text";
    case "integer":
    case "number":
      return "number";
    case "boolean":
      return "boolean";
    case "array": {
      const it = s.items;
      if (it?.enum) return "multienum";
      if (it?.type === "string") return "stringlist";
      if (it?.type === "object" && it.properties) return "objectlist";
      return "json";
    }
    case "object":
      if (s.properties && Object.keys(s.properties).length > 0) return "group";
      if (s.additionalProperties && typeof s.additionalProperties === "object") return "keyvalue";
      return "json";
    default:
      return "json";
  }
}

/** Kinds where a value may also be one expression. */
const TOGGLES: Kind[] = ["enum", "source", "number", "boolean", "json", "multienum"];

function Prop({
  name,
  schema,
  required,
  value,
  onChange,
  path,
  ctx,
  parent,
  siblings,
}: {
  name: string;
  schema: Schema;
  required: boolean;
  value: unknown;
  onChange: (v: unknown) => void;
  path: string;
  ctx: Ctx;
  parent: Schema;
  siblings: Obj;
}) {
  const kind = specialKind(name, schema, parent) ?? kindOf(schema);
  const expr = TOGGLES.includes(kind) && isExpression(value);
  const mine = issuesAt(ctx.issues, path);
  const label = humanize(name);
  const toggle = TOGGLES.includes(kind) && !ctx.disabled;
  return (
    <div className="flex flex-col gap-1.5" data-path={path}>
      <div className="flex items-center justify-between gap-2">
        <span className="text-sm font-medium">
          {label}
          {required && <span className="text-danger"> *</span>}
        </span>
        {toggle && (
          <span
            className="inline-flex rounded-(--radius-control) border border-border text-[11px]"
            role="group"
            aria-label={`${label} mode`}
          >
            <button
              type="button"
              aria-pressed={!expr}
              onClick={() => expr && onChange(undefined)}
              className={cn("px-2 py-0.5", !expr ? "bg-surface-2 font-medium" : "text-muted")}
            >
              Fixed
            </button>
            <button
              type="button"
              aria-pressed={expr}
              onClick={() => !expr && onChange("${{  }}")}
              className={cn("px-2 py-0.5", expr ? "bg-surface-2 font-medium" : "text-muted")}
            >
              Expression
            </button>
          </span>
        )}
      </div>
      {expr ? (
        <ExprField
          label={label}
          value={String(value)}
          nodeId={ctx.nodeId}
          onChange={onChange}
          single
          mono
          invalid={mine.some((i) => i.level === "error")}
        />
      ) : (
        <Control
          kind={kind}
          label={label}
          schema={schema}
          value={value}
          onChange={onChange}
          path={path}
          ctx={ctx}
          siblings={siblings}
          issues={mine}
          invalid={mine.some((i) => i.level === "error")}
        />
      )}
      {schema.description && kind !== "group" && (
        <p className="text-xs text-muted">{schema.description}</p>
      )}
      <Issues list={mine} />
    </div>
  );
}

function Control({
  kind,
  label,
  schema,
  value,
  onChange,
  path,
  ctx,
  siblings,
  issues,
  invalid,
}: {
  kind: Kind;
  label: string;
  schema: Schema;
  value: unknown;
  onChange: (v: unknown) => void;
  path: string;
  ctx: Ctx;
  siblings: Obj;
  issues: Leveled[];
  invalid: boolean;
}) {
  const ed = useEditor();
  switch (kind) {
    case "code":
      return (
        <CodeField
          label={label}
          value={typeof value === "string" ? value : ""}
          onChange={(v) => onChange(v)}
          errorLine={codeErrorLine(issues)}
          disabled={ctx.disabled}
          invalid={invalid}
        />
      );
    case "pagination":
      return (
        <PaginationEditor
          schema={schema}
          value={value}
          onChange={onChange}
          nodeId={ctx.nodeId}
          hasSplit={typeof siblings.split === "string" && siblings.split.trim() !== ""}
          disabled={ctx.disabled}
        />
      );
    case "tools":
      return (
        <ToolsEditor
          label={label}
          value={Array.isArray(value) ? (value as ToolDraft[]) : []}
          onChange={onChange}
          disabled={ctx.disabled}
          Fields={FieldList as never}
        />
      );
    case "subnode":
      return (
        <SubNodeField
          label={label}
          kind={path.endsWith(".trigger") ? "trigger" : "return_node"}
          workflowId={siblings.workflow_id}
          value={typeof value === "string" ? value : ""}
          onChange={onChange}
          disabled={ctx.disabled}
        />
      );
    case "text":
    case "longtext":
      return (
        <ExprField
          label={label}
          value={typeof value === "string" ? value : value == null ? "" : String(value)}
          nodeId={ctx.nodeId}
          onChange={onChange}
          single={kind === "text"}
          rows={kind === "longtext" ? 4 : 1}
          mono={schema["x-ui"]?.widget === "code"}
          invalid={invalid}
          disabled={ctx.disabled}
          placeholder={typeof schema.default === "string" ? schema.default : undefined}
        />
      );
    case "enum":
      return (
        <select
          aria-label={label}
          value={typeof value === "string" ? value : ""}
          disabled={ctx.disabled}
          onChange={(e) => onChange(e.target.value === "" ? undefined : e.target.value)}
          className={CONTROL}
        >
          <option value="">
            {schema.default !== undefined ? `Default (${String(schema.default)})` : "Choose…"}
          </option>
          {(schema.enum ?? []).map((o) => (
            <option key={String(o)} value={String(o)}>
              {String(o)}
            </option>
          ))}
        </select>
      );
    case "source":
      if (schema["x-ui"]?.source === "workflows") {
        return (
          <WorkflowSelect
            label={label}
            value={typeof value === "string" ? value : ""}
            onChange={onChange}
            disabled={ctx.disabled}
            excludeId={ed.workflowId}
          />
        );
      }
      return (
        <SourceField
          label={label}
          source={schema["x-ui"]?.source ?? ""}
          value={value}
          onChange={onChange}
          ctx={ctx}
        />
      );
    case "number":
      return (
        <input
          aria-label={label}
          type="number"
          value={typeof value === "number" ? value : ""}
          min={schema.minimum}
          max={schema.maximum}
          step={schema.type === "integer" ? 1 : "any"}
          disabled={ctx.disabled}
          placeholder={schema.default !== undefined ? String(schema.default) : undefined}
          onChange={(e) => onChange(e.target.value === "" ? undefined : Number(e.target.value))}
          className={CONTROL}
        />
      );
    case "boolean":
      return (
        <label className="inline-flex items-center gap-2 text-sm">
          <input
            type="checkbox"
            role="switch"
            aria-checked={typeof value === "boolean" ? value : schema.default === true}
            checked={typeof value === "boolean" ? value : schema.default === true}
            disabled={ctx.disabled}
            onChange={(e) => onChange(e.target.checked)}
            className="size-4 accent-(--accent)"
          />
          {typeof value === "boolean"
            ? value
              ? "On"
              : "Off"
            : schema.default === true
              ? "On (default)"
              : "Off (default)"}
        </label>
      );
    case "multienum": {
      const chosen = Array.isArray(value) ? (value as string[]) : [];
      return (
        <div className="flex flex-wrap gap-1.5">
          {(schema.items?.enum ?? []).map((o) => {
            const on = chosen.includes(String(o));
            return (
              <button
                key={String(o)}
                type="button"
                aria-pressed={on}
                disabled={ctx.disabled}
                onClick={() => {
                  const next = on ? chosen.filter((x) => x !== o) : [...chosen, String(o)];
                  onChange(next.length ? next : undefined);
                }}
                className={cn(
                  "rounded-full border px-2.5 py-0.5 text-xs",
                  on
                    ? "border-text/40 bg-surface-2 font-medium"
                    : "border-border text-muted hover:bg-surface-2",
                )}
              >
                {String(o)}
              </button>
            );
          })}
        </div>
      );
    }
    case "stringlist":
      return (
        <StringList
          label={label}
          value={Array.isArray(value) ? (value as string[]) : []}
          onChange={(v) => onChange(v.length ? v : undefined)}
          ctx={ctx}
        />
      );
    case "keyvalue":
      return (
        <KeyValue
          label={label}
          value={value && typeof value === "object" && !Array.isArray(value) ? (value as Obj) : {}}
          onChange={(v) => onChange(Object.keys(v).length ? v : undefined)}
          stringsOnly={
            typeof schema.additionalProperties === "object" &&
            schema.additionalProperties.type === "string"
          }
          ctx={ctx}
        />
      );
    case "rules":
      return (
        <Rules
          value={Array.isArray(value) ? (value as Rule[]) : []}
          onChange={(v) => onChange(v)}
          ctx={ctx}
        />
      );
    case "fields":
      return (
        <FieldList
          value={Array.isArray(value) ? (value as AiField[]) : []}
          onChange={(v) => onChange(v.length ? v : undefined)}
        />
      );
    case "objectlist":
      return (
        <ObjectList
          schema={schema}
          value={Array.isArray(value) ? (value as Obj[]) : []}
          onChange={onChange}
          path={path}
          ctx={ctx}
        />
      );
    case "group":
      return (
        <div className="rounded-(--radius-control) border border-border p-3">
          <ObjectForm
            schema={schema}
            value={
              value && typeof value === "object" && !Array.isArray(value) ? (value as Obj) : {}
            }
            onChange={(v) => onChange(Object.keys(v).length ? v : undefined)}
            path={path}
            ctx={ctx}
          />
        </div>
      );
    default:
      return (
        <JsonField
          label={label}
          value={value}
          onChange={onChange}
          invalid={invalid}
          disabled={ctx.disabled}
        />
      );
  }
}

// ---------- pieces ----------

function SourceField({
  label,
  source,
  value,
  onChange,
  ctx,
}: {
  label: string;
  source: string;
  value: unknown;
  onChange: (v: unknown) => void;
  ctx: Ctx;
}) {
  const ed = useEditor();
  const campaigns = useQuery({ ...q.campaigns, enabled: source === "campaigns" });
  const lists = useQuery({ ...q.lists, enabled: source === "lists" });
  const accounts = useQuery({ ...q.accounts, enabled: source === "accounts" });
  const providers = useQuery({ ...q.providers, enabled: source === "providers" });
  const secrets = useQuery({ ...aq.secrets, enabled: source === "secrets" });
  const options = useMemo(() => {
    const list: { value: string; label: string }[] = [];
    if (source === "campaigns")
      for (const c of campaigns.data?.campaigns ?? []) list.push({ value: c.id, label: c.name });
    if (source === "lists")
      for (const l of lists.data ?? []) list.push({ value: l.id, label: l.name });
    if (source === "accounts")
      for (const a of accounts.data ?? []) list.push({ value: a.id, label: a.email });
    if (source === "providers") {
      list.push(
        { value: "default_draft", label: "Default draft (strong)" },
        { value: "default_slot", label: "Default slot (cheap)" },
      );
      for (const p of providers.data ?? [])
        list.push({ value: p.name, label: `${p.name} (${p.model})` });
    }
    if (source === "secrets")
      for (const s of secrets.data ?? []) list.push({ value: s.name, label: s.name });
    return list;
  }, [source, campaigns.data, lists.data, accounts.data, providers.data, secrets.data]);
  const kinds: Record<string, string> = {
    campaigns: "campaign",
    lists: "list",
    accounts: "account",
    providers: "provider",
  };
  const vars = (ed.def.variables ?? []).filter((v) => v.kind === kinds[source]);
  const text = typeof value === "string" ? value : "";
  const known =
    options.some((o) => o.value === text) || vars.some((v) => text === `\${{ vars.${v.key} }}`);
  return (
    <select
      aria-label={label}
      value={text}
      disabled={ctx.disabled}
      onChange={(e) => onChange(e.target.value === "" ? undefined : e.target.value)}
      className={CONTROL}
    >
      <option value="">Choose…</option>
      {text && !known && <option value={text}>{text}</option>}
      {vars.length > 0 && (
        <optgroup label="Workflow variables">
          {vars.map((v) => (
            <option key={v.key} value={`\${{ vars.${v.key} }}`}>
              {v.label || v.key}
            </option>
          ))}
        </optgroup>
      )}
      <optgroup label={humanize(source)}>
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </optgroup>
    </select>
  );
}

function IconButton({
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
      className="flex size-8 shrink-0 items-center justify-center rounded-(--radius-control) text-muted hover:bg-surface-2 hover:text-text disabled:opacity-50"
    >
      {children}
    </button>
  );
}

function AddButton({
  label,
  onClick,
  disabled,
}: {
  label: string;
  onClick: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      disabled={disabled}
      className="inline-flex h-8 w-fit items-center gap-1.5 rounded-(--radius-control) border border-dashed border-border px-2.5 text-xs text-muted hover:bg-surface-2 hover:text-text disabled:opacity-50"
    >
      <Plus size={14} aria-hidden="true" /> {label}
    </button>
  );
}

function StringList({
  label,
  value,
  onChange,
  ctx,
}: {
  label: string;
  value: string[];
  onChange: (v: string[]) => void;
  ctx: Ctx;
}) {
  return (
    <div className="flex flex-col gap-1.5">
      {value.map((v, i) => (
        <div key={i} className="flex items-start gap-1">
          <div className="min-w-0 flex-1">
            <ExprField
              label={`${label} ${i + 1}`}
              value={v}
              nodeId={ctx.nodeId}
              single
              preview={false}
              onChange={(t) => onChange(value.map((x, j) => (j === i ? t : x)))}
            />
          </div>
          <IconButton label="Remove" onClick={() => onChange(value.filter((_, j) => j !== i))}>
            <Trash2 size={15} />
          </IconButton>
        </div>
      ))}
      <AddButton label="Add" onClick={() => onChange([...value, ""])} disabled={ctx.disabled} />
    </div>
  );
}

/** A value typed in a table: numbers, true/false, null and JSON stay typed; anything else is text. */
export function parseLoose(text: string, stringsOnly: boolean): unknown {
  if (stringsOnly) return text;
  const t = text.trim();
  if (t === "") return "";
  if (/^-?\d+(\.\d+)?$/.test(t) || t === "true" || t === "false" || t === "null")
    return JSON.parse(t);
  if ((t.startsWith("{") && t.endsWith("}")) || (t.startsWith("[") && t.endsWith("]"))) {
    try {
      return JSON.parse(t);
    } catch {
      return text;
    }
  }
  return text;
}

const showLoose = (v: unknown): string => (typeof v === "string" ? v : JSON.stringify(v));

function KeyValue({
  label,
  value,
  onChange,
  stringsOnly,
  ctx,
}: {
  label: string;
  value: Obj;
  onChange: (v: Obj) => void;
  stringsOnly: boolean;
  ctx: Ctx;
}) {
  const [rows, setRows] = useState<{ k: string; v: string }[]>(() =>
    Object.entries(value).map(([k, v]) => ({ k, v: showLoose(v) })),
  );
  const sig = JSON.stringify(value);
  const [emitted, setEmitted] = useState(sig);
  useEffect(() => {
    // the draft changed from outside (undo, restore): show it
    if (sig !== emitted) {
      setRows(Object.entries(value).map(([k, v]) => ({ k, v: showLoose(v) })));
      setEmitted(sig);
    }
  }, [sig]);
  function push(next: { k: string; v: string }[]) {
    setRows(next);
    const out: Obj = {};
    for (const r of next)
      if (r.k.trim() !== "" && !(r.k in out)) out[r.k] = parseLoose(r.v, stringsOnly);
    setEmitted(JSON.stringify(out));
    onChange(out);
  }
  return (
    <div className="flex flex-col gap-1.5">
      {rows.map((r, i) => (
        <div key={i} className="flex items-start gap-1">
          <input
            aria-label={`${label} key ${i + 1}`}
            value={r.k}
            placeholder="key"
            disabled={ctx.disabled}
            onChange={(e) => push(rows.map((x, j) => (j === i ? { ...x, k: e.target.value } : x)))}
            className={cn(CONTROL, "w-2/5 shrink-0 font-mono text-[13px]")}
          />
          <div className="min-w-0 flex-1">
            <ExprField
              label={`${label} value ${i + 1}`}
              value={r.v}
              nodeId={ctx.nodeId}
              single
              mono
              onChange={(t) => push(rows.map((x, j) => (j === i ? { ...x, v: t } : x)))}
            />
          </div>
          <IconButton label="Remove" onClick={() => push(rows.filter((_, j) => j !== i))}>
            <Trash2 size={15} />
          </IconButton>
        </div>
      ))}
      <AddButton
        label="Add a row"
        onClick={() => setRows([...rows, { k: "", v: "" }])}
        disabled={ctx.disabled}
      />
    </div>
  );
}

interface Rule {
  left?: unknown;
  op?: string;
  right?: unknown;
}

const OPS: [string, string][] = [
  ["equals", "equals"],
  ["not_equals", "does not equal"],
  ["contains", "contains"],
  ["not_contains", "does not contain"],
  ["starts_with", "starts with"],
  ["ends_with", "ends with"],
  ["gt", "is greater than"],
  ["gte", "is at least"],
  ["lt", "is less than"],
  ["lte", "is at most"],
  ["is_empty", "is empty"],
  ["is_not_empty", "is not empty"],
  ["is_true", "is true"],
  ["is_false", "is false"],
  ["matches", "matches (regex)"],
];
const UNARY = new Set(["is_empty", "is_not_empty", "is_true", "is_false"]);

function Rules({
  value,
  onChange,
  ctx,
}: {
  value: Rule[];
  onChange: (v: Rule[]) => void;
  ctx: Ctx;
}) {
  const set = (i: number, patch: Rule) =>
    onChange(value.map((r, j) => (j === i ? { ...r, ...patch } : r)));
  return (
    <div className="flex flex-col gap-2">
      {value.map((r, i) => (
        <div
          key={i}
          className="flex flex-col gap-1.5 rounded-(--radius-control) border border-border p-2"
        >
          <div className="flex items-start gap-1">
            <div className="min-w-0 flex-1">
              <ExprField
                label={`Rule ${i + 1} value`}
                value={showLoose(r.left ?? "")}
                nodeId={ctx.nodeId}
                single
                preview={false}
                onChange={(t) => set(i, { left: t })}
                placeholder="${{ json.value }}"
              />
            </div>
            <IconButton
              label="Remove rule"
              onClick={() => onChange(value.filter((_, j) => j !== i))}
            >
              <Trash2 size={15} />
            </IconButton>
          </div>
          <select
            aria-label={`Rule ${i + 1} operator`}
            value={r.op ?? "equals"}
            onChange={(e) => set(i, { op: e.target.value })}
            className={CONTROL}
          >
            {OPS.map(([v, l]) => (
              <option key={v} value={v}>
                {l}
              </option>
            ))}
          </select>
          {!UNARY.has(r.op ?? "equals") && (
            <ExprField
              label={`Rule ${i + 1} compare to`}
              value={showLoose(r.right ?? "")}
              nodeId={ctx.nodeId}
              single
              preview={false}
              onChange={(t) => set(i, { right: t })}
            />
          )}
        </div>
      ))}
      <AddButton
        label="Add a rule"
        onClick={() => onChange([...value, { left: "", op: "equals", right: "" }])}
        disabled={ctx.disabled}
      />
    </div>
  );
}

interface AiField {
  key: string;
  type: string;
  description?: string;
  required?: boolean;
}

const FIELD_TYPES = ["string", "number", "boolean", "array", "object"];

function FieldList({ value, onChange }: { value: AiField[]; onChange: (v: AiField[]) => void }) {
  const set = (i: number, patch: Partial<AiField>) =>
    onChange(value.map((f, j) => (j === i ? { ...f, ...patch } : f)));
  return (
    <div className="flex flex-col gap-2">
      {value.map((f, i) => (
        <div
          key={i}
          className="flex flex-col gap-1.5 rounded-(--radius-control) border border-border p-2"
        >
          <div className="flex items-center gap-1">
            <input
              aria-label={`Field ${i + 1} key`}
              value={f.key}
              placeholder="key"
              onChange={(e) => set(i, { key: e.target.value })}
              className={cn(CONTROL, "min-w-0 flex-1 font-mono text-[13px]")}
            />
            <select
              aria-label={`Field ${i + 1} type`}
              value={f.type}
              onChange={(e) => set(i, { type: e.target.value })}
              className={cn(CONTROL, "w-28 shrink-0")}
            >
              {FIELD_TYPES.map((t) => (
                <option key={t}>{t}</option>
              ))}
            </select>
            <IconButton
              label="Remove field"
              onClick={() => onChange(value.filter((_, j) => j !== i))}
            >
              <Trash2 size={15} />
            </IconButton>
          </div>
          <input
            aria-label={`Field ${i + 1} description`}
            value={f.description ?? ""}
            placeholder="What the model should put here"
            onChange={(e) => set(i, { description: e.target.value })}
            className={CONTROL}
          />
          <label className="inline-flex items-center gap-2 text-xs text-muted">
            <input
              type="checkbox"
              checked={f.required !== false}
              onChange={(e) => set(i, { required: e.target.checked })}
              className="accent-(--accent)"
            />
            Required
          </label>
        </div>
      ))}
      <AddButton
        label="Add a field"
        onClick={() =>
          onChange([...value, { key: "", type: "string", description: "", required: true }])
        }
        disabled={value.length >= 30}
      />
    </div>
  );
}

function ObjectList({
  schema,
  value,
  onChange,
  path,
  ctx,
}: {
  schema: Schema;
  value: Obj[];
  onChange: (v: unknown) => void;
  path: string;
  ctx: Ctx;
}) {
  const item = schema.items as Schema;
  const max = schema.maxItems ?? 50;
  return (
    <div className="flex flex-col gap-2">
      {value.map((v, i) => (
        <div key={i} className="rounded-(--radius-control) border border-border p-3">
          <div className="mb-2 flex items-center justify-between">
            <span className="text-xs font-medium text-muted">#{i + 1}</span>
            <IconButton label="Remove" onClick={() => onChange(value.filter((_, j) => j !== i))}>
              <Trash2 size={15} />
            </IconButton>
          </div>
          <ObjectForm
            schema={item}
            value={v}
            path={`${path}[${i}]`}
            ctx={ctx}
            onChange={(next) => onChange(value.map((x, j) => (j === i ? next : x)))}
          />
        </div>
      ))}
      <AddButton
        label="Add"
        onClick={() => onChange([...value, {}])}
        disabled={value.length >= max || ctx.disabled}
      />
    </div>
  );
}

/** Anything else: a JSON text. */
export function JsonField({
  label,
  value,
  onChange,
  invalid,
  disabled,
  rows = 4,
}: {
  label: string;
  value: unknown;
  onChange: (v: unknown) => void;
  invalid?: boolean;
  disabled?: boolean | undefined;
  rows?: number;
}) {
  const canon = value === undefined ? "" : JSON.stringify(value, null, 2);
  const [text, setText] = useState(canon);
  const [bad, setBad] = useState<string | null>(null);
  useEffect(() => {
    // follow changes from outside (undo, restore), but not our own typing
    try {
      if (
        text.trim() === ""
          ? value === undefined
          : JSON.stringify(JSON.parse(text)) === JSON.stringify(value)
      )
        return;
    } catch {
      return;
    }
    setText(canon);
  }, [canon]);
  return (
    <div className="flex flex-col gap-1">
      <textarea
        aria-label={label}
        value={text}
        rows={rows}
        disabled={disabled}
        spellCheck={false}
        onChange={(e) => {
          setText(e.target.value);
          if (e.target.value.trim() === "") {
            setBad(null);
            onChange(undefined);
            return;
          }
          try {
            onChange(JSON.parse(e.target.value));
            setBad(null);
          } catch (err) {
            setBad(err instanceof Error ? err.message : "Not valid JSON.");
          }
        }}
        className={cn(
          "w-full rounded-(--radius-control) border bg-(--input-bg) px-3 py-2 font-mono text-[13px]",
          invalid || bad ? "border-danger" : "border-border",
        )}
      />
      {bad && <p className="text-xs text-danger">Not valid JSON yet: {bad}</p>}
    </div>
  );
}
