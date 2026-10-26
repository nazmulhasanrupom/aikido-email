/** What a node gave, from a pin, a run or the catalog example. Also the sample data for autocomplete. */
import { findNode, isTrigger, parentOf, portOf, upstream } from "./defs";
import type { NodeSpec, Obj, RunDetail, WfDef } from "./types";

export interface NodeData {
  source: "pin" | "run" | "example" | "none";
  /** Items per output port. */
  ports: Record<string, Obj[]>;
  /** Items the run had but did not send (the page cuts at 20 per port). */
  more: Record<string, number>;
  status?: string;
}

const asItems = (example: unknown): Obj[] =>
  example && typeof example === "object" && !Array.isArray(example) ? [example as Obj] : [];

function latestRow(run: RunDetail | null, id: string) {
  let best: RunDetail["nodes"][number] | undefined;
  for (const n of run?.nodes ?? []) if (n.node_id === id && (!best || n.seq > best.seq)) best = n;
  return best;
}

export function dataOf(
  def: WfDef,
  specs: Map<string, NodeSpec>,
  run: RunDetail | null,
  id: string,
  useExample = true,
): NodeData {
  const pin = def.pins?.[id];
  if (pin?.length) return { source: "pin", ports: { main: pin }, more: {} };
  const row = latestRow(run, id);
  if (row?.output) {
    return {
      source: "run",
      ports: row.output,
      more: row.truncated_items ?? {},
      status: row.status,
    };
  }
  if (run && id === run.trigger_node_id && run.trigger_item) {
    return { source: "run", ports: { main: [run.trigger_item] }, more: {} };
  }
  const node = findNode(def, id);
  const spec = node ? specs.get(node.type) : undefined;
  if (useExample && spec)
    return { source: "example", ports: { main: asItems(spec.output_example) }, more: {} };
  return { source: "none", ports: {}, more: {} };
}

/** The first item of a port (or of any port). */
function firstItem(d: NodeData, port?: string): Obj | undefined {
  if (port && d.ports[port]?.[0]) return d.ports[port]?.[0];
  for (const items of Object.values(d.ports)) if (items[0]) return items[0];
  return undefined;
}

/** The data an expression field in node `id` can read, as one sample object. */
export function sampleContext(
  def: WfDef,
  specs: Map<string, NodeSpec>,
  run: RunDetail | null,
  id: string,
): Obj {
  const ups = upstream(def, id);
  const nodes: Obj = {};
  let trigger: Obj = {};
  for (const u of ups) {
    const d = dataOf(def, specs, run, u);
    const first = firstItem(d, "main");
    nodes[u] = { json: first ?? {}, items: Object.values(d.ports).flat().slice(0, 3) };
    const n = findNode(def, u);
    if (n && isTrigger(n.type) && first) trigger = first;
  }
  const edge = parentOf(def, id);
  const parent = edge ? firstItem(dataOf(def, specs, run, edge.from), portOf(edge)) : undefined;
  const vars: Obj = {};
  for (const v of def.variables ?? []) vars[v.key] = v.value ?? "";
  return {
    json: parent ?? (isTrigger(findNode(def, id)?.type ?? "") ? trigger : {}),
    trigger,
    nodes,
    vars,
    run: { id: "", mode: "test", started_at: "", item_index: 0 },
    workflow: { id: "", name: def.name, version: null },
  };
}

// ---------- paths for autocomplete ----------

export interface Suggestion {
  /** What is inserted. */
  text: string;
  /** What the list shows (default: text). */
  label?: string;
  hint?: string;
  /** More names can follow after a dot. */
  more?: boolean;
}

export const FILTERS: Suggestion[] = [
  { text: "default('')", hint: "value when missing" },
  { text: "lower" },
  { text: "upper" },
  { text: "title" },
  { text: "trim" },
  { text: "replace('a', 'b')" },
  { text: "length" },
  { text: "first" },
  { text: "last" },
  { text: "join(', ')" },
  { text: "split(',')" },
  { text: "tojson" },
  { text: "truncate(200)" },
  { text: "int" },
  { text: "float" },
  { text: "round" },
  { text: "from_json", hint: "text to object" },
  { text: "domain", hint: "ana@acme.com -> acme.com" },
  { text: "first_word" },
  { text: "strip_html" },
  { text: "words(50)" },
  { text: "slug" },
  { text: "date('%Y-%m-%d')" },
  { text: "add_days(1)" },
  { text: "add_hours(1)" },
  { text: "pick('a', 'b')" },
  { text: "omit('a')" },
  { text: "md5" },
  { text: "sha256" },
];

const ROOTS: Suggestion[] = [
  { text: "json", hint: "the item this node works on", more: true },
  { text: "trigger", hint: "the item that started the run", more: true },
  { text: "nodes", hint: "an earlier node", more: true },
  { text: "vars", hint: "workflow variables", more: true },
  { text: "run", hint: "run facts", more: true },
  { text: "workflow", hint: "workflow facts", more: true },
  { text: "now()", hint: "current time" },
  { text: "secret('')", hint: "a secret value" },
  { text: "uuid()" },
  { text: "get(json, 'a.b', '')", hint: "safe deep read" },
];

function typeHint(v: unknown): string {
  if (Array.isArray(v)) return `list (${v.length})`;
  if (v === null) return "empty";
  if (typeof v === "object") return "object";
  if (typeof v === "string") return v.length > 24 ? `"${v.slice(0, 24)}…"` : `"${v}"`;
  return String(v);
}

function keysOf(v: unknown): Suggestion[] {
  if (Array.isArray(v)) {
    const first = v[0];
    return first && typeof first === "object"
      ? keysOf(first).map((k) => ({ ...k, hint: `of item 0: ${k.hint}` }))
      : [];
  }
  if (!v || typeof v !== "object") return [];
  return Object.entries(v as Obj).map(([k, val]) => ({
    text: k,
    hint: typeHint(val),
    more: val !== null && typeof val === "object",
  }));
}

/** What can follow the text typed so far inside `${{ }}`. `before` is the text of the expression up to the caret. */
export function suggest(
  before: string,
  sample: Obj,
  extraRoots: readonly Suggestion[] = [],
): { from: number; items: Suggestion[] } {
  // after a pipe: a filter name
  const pipe = /\|\s*([A-Za-z_]*)$/.exec(before);
  if (pipe) {
    const word = pipe[1] ?? "";
    return {
      from: before.length - word.length,
      items: FILTERS.filter((f) => f.text.startsWith(word)),
    };
  }
  const m = /([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z0-9_]*)*\.?)$/.exec(before);
  const token = m?.[1] ?? "";
  const start = before.length - token.length;
  if (!token.includes(".")) {
    return {
      from: start,
      items: [...extraRoots, ...ROOTS].filter((r) => r.text.startsWith(token)),
    };
  }
  const parts = token.split(".");
  const partial = parts.pop() ?? "";
  let cur: unknown = sample;
  for (const p of parts) {
    if (cur === null || typeof cur !== "object") return { from: start, items: [] };
    cur = Array.isArray(cur) ? (/^\d+$/.test(p) ? cur[Number(p)] : cur[0]?.[p]) : (cur as Obj)[p];
  }
  const items = keysOf(cur).filter((k) => k.text.toLowerCase().startsWith(partial.toLowerCase()));
  return { from: before.length - partial.length, items };
}

/** All dotted paths of a value (for the Input tree drag and for pickers). Lists use their first item. */
export function pathsOf(v: unknown, prefix = "", depth = 0): { path: string; value: unknown }[] {
  if (depth > 5 || v === null || typeof v !== "object") return [];
  const out: { path: string; value: unknown }[] = [];
  const entries = Array.isArray(v)
    ? v[0] !== undefined
      ? [["0", v[0]] as const]
      : []
    : Object.entries(v as Obj);
  for (const [k, val] of entries) {
    const p = prefix ? `${prefix}.${k}` : k;
    out.push({ path: p, value: val });
    out.push(...pathsOf(val, p, depth + 1));
  }
  return out;
}
