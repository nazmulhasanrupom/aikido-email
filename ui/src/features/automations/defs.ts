/**
 * Plain functions on a workflow definition: add, connect, remove, copy, paste, compare.
 * Nothing here touches React or the network, so the unit tests can run them.
 */
import type { NodeSpec, Obj, Schema, WfDef, WfEdge, WfNode } from "./types";

export const RESERVED = new Set([
  "trigger",
  "json",
  "item",
  "items",
  "nodes",
  "vars",
  "run",
  "workflow",
  "now",
  "secret",
]);

export const ID_RE = /^[a-z][a-z0-9_]{0,39}$/;

/** `web.fetch_page` -> `web_fetch_page`, `trigger.lead_created` -> `on_lead_created`, `aikido.lead.get` -> `lead_get`. */
export function slugBase(type: string): string {
  const parts = type.split(".");
  if (parts[0] === "aikido") parts.shift();
  const head = parts[0] === "trigger" ? "on" : null;
  const text = (head ? [head, ...parts.slice(1)] : parts).join("_");
  const clean = text.toLowerCase().replace(/[^a-z0-9_]/g, "_");
  return /^[a-z]/.test(clean) ? clean.slice(0, 34) : `n_${clean}`.slice(0, 34);
}

/** A free node id that follows the rules (`^[a-z][a-z0-9_]{0,39}$`, not reserved). */
export function uniqueId(base: string, taken: Iterable<string>): string {
  const used = new Set(taken);
  let root = base.toLowerCase().replace(/[^a-z0-9_]/g, "_");
  if (!/^[a-z]/.test(root)) root = `n_${root}`;
  root = root.slice(0, 34);
  if (!used.has(root) && !RESERVED.has(root)) return root;
  for (let i = 2; i < 10000; i++) {
    const id = `${root}_${i}`;
    if (!used.has(id)) return id;
  }
  return `${root}_${Date.now() % 100000}`;
}

/** A readable name from a text: `Fetch site` -> `fetch_site`. */
export function idFromName(name: string, taken: Iterable<string>): string {
  const base = name
    .trim()
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "_")
    .replace(/^_+|_+$/g, "");
  return uniqueId(base || "node", taken);
}

export function randomPath(len = 16): string {
  const chars = "abcdefghijklmnopqrstuvwxyz0123456789";
  const bytes = new Uint8Array(len);
  crypto.getRandomValues(bytes);
  return Array.from(bytes, (b) => chars[b % chars.length]).join("");
}

/** The params a new node starts with: the defaults of its schema, and the first choice of a required list. */
export function defaultParams(spec: NodeSpec): Obj {
  const out: Obj = {};
  const schema = spec.params_schema;
  const required = new Set(schema.required ?? []);
  for (const [key, prop] of Object.entries(schema.properties ?? {})) {
    if (prop.default !== undefined) out[key] = structuredClone(prop.default);
    else if (required.has(key) && prop.enum && prop.enum.length > 0) out[key] = prop.enum[0];
  }
  switch (spec.type) {
    case "trigger.schedule":
      return { mode: "interval", every: 1, unit: "hours", ...out };
    case "trigger.cron":
      return { expression: "0 9 * * *", ...out };
    case "trigger.webhook":
      // both URLs are the same only when the path is in the node
      return { path: randomPath(), method: "POST", ...out };
    case "flow.wait":
      return { mode: "duration", amount: 1, unit: "minutes", ...out };
    case "ai.agent":
      return { provider: "default_draft", prompt: "", output: { format: "text" }, ...out };
    case "flow.if":
    case "flow.filter":
      return { conditions: [], ...out };
    case "flow.switch":
      return { cases: [{ conditions: [] }, { conditions: [] }], ...out };
    case "data.set":
      return { fields: {}, ...out };
    default:
      return out;
  }
}

export function isTrigger(type: string): boolean {
  return type.startsWith("trigger.");
}

export function findNode(def: WfDef, id: string | null | undefined): WfNode | undefined {
  return id ? def.nodes.find((n) => n.id === id) : undefined;
}

/** The output ports a node shows. Switch has one port per case. */
export function outPorts(node: WfNode, spec: NodeSpec | undefined): string[] {
  let ports: string[];
  if (node.type === "flow.switch") {
    const n = Array.isArray(node.params?.cases) ? (node.params.cases as unknown[]).length : 2;
    ports = [...Array.from({ length: Math.max(1, Math.min(20, n)) }, (_, i) => `case_${i + 1}`), "default"];
  } else {
    ports = spec ? [...spec.ports] : ["main"];
  }
  if (node.on_error === "error_port" && !ports.includes("error")) ports.push("error");
  return ports;
}

export function portLabel(port: string): string {
  if (port === "main") return "";
  return port.replace(/^case_(\d+)$/, "case $1").replace(/_/g, " ");
}

export const portOf = (e: WfEdge): string => e.port ?? "main";

/** Does an edge `from -> to` close a loop? */
export function wouldCycle(def: WfDef, from: string, to: string): boolean {
  if (from === to) return true;
  const next = new Map<string, string[]>();
  for (const e of def.edges) next.set(e.from, [...(next.get(e.from) ?? []), e.to]);
  const seen = new Set<string>();
  const stack = [to];
  while (stack.length) {
    const n = stack.pop() as string;
    if (n === from) return true;
    if (seen.has(n)) continue;
    seen.add(n);
    stack.push(...(next.get(n) ?? []));
  }
  return false;
}

export type ConnectProblem = "self" | "to_trigger" | "duplicate" | "cycle" | "missing";

export function connectProblem(
  def: WfDef,
  from: string,
  port: string,
  to: string,
): ConnectProblem | null {
  const a = findNode(def, from);
  const b = findNode(def, to);
  if (!a || !b) return "missing";
  if (from === to) return "self";
  if (isTrigger(b.type)) return "to_trigger";
  if (def.edges.some((e) => e.from === from && portOf(e) === port && e.to === to)) return "duplicate";
  if (wouldCycle(def, from, to)) return "cycle";
  return null;
}

export function edgeOf(from: string, port: string, to: string): WfEdge {
  return port === "main" ? { from, to } : { from, port, to };
}

export function connect(def: WfDef, from: string, port: string, to: string): WfDef {
  if (connectProblem(def, from, port, to)) return def;
  return { ...def, edges: [...def.edges, edgeOf(from, port, to)] };
}

export function disconnect(def: WfDef, edge: WfEdge): WfDef {
  const p = portOf(edge);
  return {
    ...def,
    edges: def.edges.filter((e) => !(e.from === edge.from && portOf(e) === p && e.to === edge.to)),
  };
}

export function removeNodes(def: WfDef, ids: string[]): WfDef {
  const gone = new Set(ids);
  const pins = def.pins ? Object.fromEntries(Object.entries(def.pins).filter(([k]) => !gone.has(k))) : undefined;
  const next: WfDef = {
    ...def,
    nodes: def.nodes.filter((n) => !gone.has(n.id)),
    edges: def.edges.filter((e) => !gone.has(e.from) && !gone.has(e.to)),
  };
  if (pins) next.pins = pins;
  return next;
}

export interface AddOptions {
  position?: { x: number; y: number };
  /** Connect from this node's port. */
  after?: { id: string; port: string };
  name?: string;
}

/** Add a node of this type. Returns the new definition and the id of the node. */
export function addNode(def: WfDef, spec: NodeSpec, opts: AddOptions = {}): { def: WfDef; id: string } {
  const id = uniqueId(slugBase(spec.type), def.nodes.map((n) => n.id));
  const node: WfNode = { id, type: spec.type, name: opts.name ?? spec.label, params: defaultParams(spec) };
  if (opts.position) node.position = { x: Math.round(opts.position.x), y: Math.round(opts.position.y) };
  let next: WfDef = { ...def, nodes: [...def.nodes, node] };
  if (opts.after && !isTrigger(spec.type)) next = connect(next, opts.after.id, opts.after.port, id);
  return { def: next, id };
}

/** Put a node in the middle of an edge. */
export function insertOnEdge(def: WfDef, edge: WfEdge, nodeId: string): WfDef {
  const node = findNode(def, nodeId);
  if (!node || isTrigger(node.type)) return def;
  const p = portOf(edge);
  const without = disconnect(def, edge);
  // do not let the middle node make a loop
  if (wouldCycle(without, edge.from, nodeId) || wouldCycle(without, nodeId, edge.to)) return def;
  let next = connect(without, edge.from, p, nodeId);
  next = connect(next, nodeId, "main", edge.to);
  return next;
}

/** The nodes that lead to `id`, nearest first. */
export function upstream(def: WfDef, id: string): string[] {
  const out: string[] = [];
  const seen = new Set<string>([id]);
  let layer = [id];
  while (layer.length) {
    const next: string[] = [];
    for (const n of layer) {
      for (const e of def.edges) {
        if (e.to === n && !seen.has(e.from)) {
          seen.add(e.from);
          next.push(e.from);
          out.push(e.from);
        }
      }
    }
    layer = next;
  }
  return out;
}

/** The first parent of a node (the one whose output is `json`). */
export function parentOf(def: WfDef, id: string): WfEdge | undefined {
  return def.edges.find((e) => e.to === id);
}

// ---------- copy and paste ----------

export interface Fragment {
  nodes: WfNode[];
  edges: WfEdge[];
}

/** The selected nodes and the edges between them. */
export function fragmentOf(def: WfDef, ids: string[]): Fragment {
  const keep = new Set(ids);
  return {
    nodes: structuredClone(def.nodes.filter((n) => keep.has(n.id))),
    edges: structuredClone(def.edges.filter((e) => keep.has(e.from) && keep.has(e.to))),
  };
}

function rewriteRefs(value: unknown, map: Map<string, string>): unknown {
  if (typeof value === "string") {
    if (!value.includes("${")) return value;
    let out = value;
    for (const [from, to] of map) {
      if (from === to) continue;
      out = out.replace(new RegExp(`\\bnodes\\.${from}\\b`, "g"), `nodes.${to}`);
    }
    return out;
  }
  if (Array.isArray(value)) return value.map((v) => rewriteRefs(v, map));
  if (value && typeof value === "object") {
    return Object.fromEntries(
      Object.entries(value).map(([k, v]) => [k, rewriteRefs(v, map)]),
    );
  }
  return value;
}

/** Add copies of the fragment's nodes with new ids. Expressions that read a copied node follow it. */
export function pasteFragment(
  def: WfDef,
  frag: Fragment,
  offset = { x: 40, y: 40 },
): { def: WfDef; ids: string[] } {
  const taken = new Set(def.nodes.map((n) => n.id));
  const map = new Map<string, string>();
  for (const n of frag.nodes) {
    const id = uniqueId(n.id, taken);
    taken.add(id);
    map.set(n.id, id);
  }
  const nodes = frag.nodes.map((n) => {
    const copy = structuredClone(n);
    copy.id = map.get(n.id) as string;
    if (copy.params) copy.params = rewriteRefs(copy.params, map) as Obj;
    if (copy.position) copy.position = { x: copy.position.x + offset.x, y: copy.position.y + offset.y };
    return copy;
  });
  const edges = frag.edges.map((e) => edgeOf(map.get(e.from) as string, portOf(e), map.get(e.to) as string));
  return { def: { ...def, nodes: [...def.nodes, ...nodes], edges: [...def.edges, ...edges] }, ids: nodes.map((n) => n.id) };
}

/** Text from the clipboard that is a workflow file or a fragment (a list of nodes). */
export function fragmentFromText(text: string): Fragment | null {
  let v: unknown;
  try {
    v = JSON.parse(text.replace(/^```(?:json)?\s*|\s*```$/g, ""));
  } catch {
    return null;
  }
  const o = (v && typeof v === "object" ? ((v as Obj).workflow ?? v) : null) as Obj | null;
  if (!o || !Array.isArray(o.nodes)) return null;
  const nodes = (o.nodes as unknown[]).filter(
    (n): n is WfNode =>
      !!n && typeof n === "object" && typeof (n as WfNode).id === "string" && typeof (n as WfNode).type === "string",
  );
  if (nodes.length === 0) return null;
  const ids = new Set(nodes.map((n) => n.id));
  const edges = (Array.isArray(o.edges) ? (o.edges as WfEdge[]) : []).filter(
    (e) => e && ids.has(e.from) && ids.has(e.to),
  );
  return { nodes, edges };
}

// ---------- compare ----------

export interface DefDiff {
  added: string[];
  removed: string[];
  changed: string[];
  edgesAdded: number;
  edgesRemoved: number;
}

function nodeKey(n: WfNode): string {
  const { position: _p, ...rest } = n;
  return JSON.stringify(rest);
}

const edgeKey = (e: WfEdge) => `${e.from}:${portOf(e)}>${e.to}`;

/** What differs between two definitions. Positions do not count. */
export function diffDefs(before: WfDef | null, after: WfDef): DefDiff {
  const a = new Map((before?.nodes ?? []).map((n) => [n.id, n]));
  const b = new Map(after.nodes.map((n) => [n.id, n]));
  const added = [...b.keys()].filter((id) => !a.has(id));
  const removed = [...a.keys()].filter((id) => !b.has(id));
  const changed = [...b.keys()].filter((id) => {
    const old = a.get(id);
    return old && nodeKey(old) !== nodeKey(b.get(id) as WfNode);
  });
  const ea = new Set((before?.edges ?? []).map(edgeKey));
  const eb = new Set(after.edges.map(edgeKey));
  return {
    added,
    removed,
    changed,
    edgesAdded: [...eb].filter((k) => !ea.has(k)).length,
    edgesRemoved: [...ea].filter((k) => !eb.has(k)).length,
  };
}

// ---------- schema helpers ----------

export function propsOf(schema: Schema): [string, Schema][] {
  return Object.entries(schema.properties ?? {});
}

/** Which values of a param are expressions. */
export function isExpression(v: unknown): v is string {
  return typeof v === "string" && /\$\{[{%#]/.test(v);
}

/** Is this param value exactly one `${{ }}` and nothing else? */
export function wholeExpression(v: unknown): string | null {
  if (typeof v !== "string") return null;
  const m = /^\s*\$\{\{([\s\S]*)\}\}\s*$/.exec(v);
  if (!m) return null;
  return m[1]?.includes("}}") ? null : (m[1] ?? "").trim();
}

/** Set a value at a path like `output.format` inside params. Empty values remove the key. */
export function setPath(obj: Obj, path: string[], value: unknown): Obj {
  const [head, ...rest] = path;
  if (head === undefined) return obj;
  const copy: Obj = { ...obj };
  if (rest.length === 0) {
    if (value === undefined) delete copy[head];
    else copy[head] = value;
    return copy;
  }
  const child = copy[head];
  copy[head] = setPath(child && typeof child === "object" && !Array.isArray(child) ? (child as Obj) : {}, rest, value);
  return copy;
}

/** Read a deep value by path pieces. */
export function getPath(value: unknown, path: (string | number)[]): unknown {
  let cur: unknown = value;
  for (const p of path) {
    if (cur === null || cur === undefined) return undefined;
    cur = (cur as Record<string | number, unknown>)[p];
  }
  return cur;
}

// ---------- rename and clean up ----------

/** Give a node a new id. Edges, pins and `nodes.<id>` in every expression follow. */
export function renameNode(def: WfDef, oldId: string, newId: string): WfDef {
  if (oldId === newId || !ID_RE.test(newId) || RESERVED.has(newId) || def.nodes.some((n) => n.id === newId)) return def;
  const map = new Map([[oldId, newId]]);
  const pins = def.pins
    ? Object.fromEntries(Object.entries(def.pins).map(([k, v]) => [k === oldId ? newId : k, v]))
    : undefined;
  const next: WfDef = {
    ...def,
    nodes: def.nodes.map((n) => {
      const copy: WfNode = { ...n, id: n.id === oldId ? newId : n.id };
      if (n.params) copy.params = rewriteRefs(n.params, map) as Obj;
      return copy;
    }),
    edges: def.edges.map((e) => ({ ...e, from: e.from === oldId ? newId : e.from, to: e.to === oldId ? newId : e.to })),
  };
  if (pins) next.pins = pins;
  return next;
}

/** Remove edges that leave a port the node no longer has (a Switch with fewer cases, a node without the error port). */
export function pruneEdges(def: WfDef, nodeId: string, spec: NodeSpec | undefined): WfDef {
  const node = findNode(def, nodeId);
  if (!node) return def;
  const ports = new Set(outPorts(node, spec));
  const edges = def.edges.filter((e) => e.from !== nodeId || ports.has(portOf(e)));
  return edges.length === def.edges.length ? def : { ...def, edges };
}

/** The signature of what a publish would freeze (positions, notes and pins do not count). */
export function publishSignature(def: WfDef): string {
  return JSON.stringify({
    name: def.name,
    description: def.description ?? "",
    variables: def.variables ?? [],
    settings: def.settings ?? {},
    nodes: def.nodes.map(({ position: _p, ...n }) => n),
    edges: def.edges.map((e) => `${e.from}:${portOf(e)}>${e.to}`).sort(),
  });
}
