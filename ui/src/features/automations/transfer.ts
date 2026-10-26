import type { ApiProblem } from "@/api/errors";
import { pasteFragment, uniqueId } from "./defs";
import type {
  Issue,
  Obj,
  Validation,
  WfDef,
  WfNode,
  WfSticky,
  WfVariable,
  WorkflowDetail,
} from "./types";

/** Import, export, templates: the shapes of the routes and the pure logic around them. */

export interface NeedsYou {
  kind: "variable" | "provider" | "secret" | "app" | string;
  key?: string;
  var_kind?: string;
  name?: string;
  node_id?: string;
  hint?: string;
  message: string;
}

export interface ImportReport {
  fixed: string[];
  needs_you: NeedsYou[];
  warnings: string[];
  validation: Validation;
  source?: {
    exported_at?: string;
    app_version?: string;
    exported_from_version?: number | null;
  } | null;
}

export interface ImportReply {
  workflow: WorkflowDetail;
  report: ImportReport;
}

export interface ImportCheckReply {
  definition: WfDef;
  report: ImportReport;
}

/** Exactly one of `text`, `definition`, `url`. */
export type ImportSource = { text: string } | { definition: unknown } | { url: string };

export interface TemplateInfo {
  id: string;
  name: string;
  description: string;
  node_count: number;
  node_types: string[];
  variables: { key: string; label: string; kind: string; required: boolean }[];
  secrets: string[];
}

// ---------- the report of the last import, kept until the editor shows it ----------

const pending = new Map<string, ImportReport>();

export function keepReport(workflowId: string, report: ImportReport): void {
  pending.set(workflowId, report);
}

/** The report waiting for this workflow (once). */
export function takeReport(workflowId: string): ImportReport | null {
  const r = pending.get(workflowId) ?? null;
  pending.delete(workflowId);
  return r;
}

/** True when there is something in the report worth a panel. */
export function reportHasNews(r: ImportReport): boolean {
  return (
    r.fixed.length > 0 ||
    r.warnings.length > 0 ||
    r.needs_you.length > 0 ||
    r.validation.errors.length > 0 ||
    r.validation.warnings.length > 0
  );
}

// ---------- report -> actions ----------

export type ActionKind = "pick" | "id" | "secret" | "provider" | "app";

export interface ReportAction {
  /** Stable key for lists. */
  id: string;
  kind: ActionKind;
  message: string;
  /** `pick`, `id`: the variable key. `provider`: the node id. `secret`, `app`: the name. */
  key: string;
  /** `pick`: what is picked (`campaign`, `list`, `account`, `provider`, `custom_field`). */
  varKind?: string;
  hint?: string;
  done: boolean;
}

const PICK_KINDS = new Set(["campaign", "list", "account", "provider", "custom_field"]);

function filled(v: unknown): boolean {
  return v !== null && v !== undefined && v !== "";
}

/** One action per `needs_you` item. `done` is read from the draft and the secrets, so the list shrinks as the person works. */
export function actionsFor(
  report: ImportReport,
  def: WfDef,
  secrets: ReadonlySet<string>,
): ReportAction[] {
  const out: ReportAction[] = [];
  report.needs_you.forEach((n, i) => {
    const id = `${i}:${n.kind}:${n.key ?? n.name ?? n.node_id ?? ""}`;
    const base = { id, message: n.message, ...(n.hint ? { hint: n.hint } : {}) };
    if (n.kind === "variable" && n.key) {
      const v = (def.variables ?? []).find((x) => x.key === n.key);
      const vk = n.var_kind ?? v?.kind ?? "text";
      out.push({
        ...base,
        kind: PICK_KINDS.has(vk) ? "pick" : "id",
        key: n.key,
        varKind: vk,
        done: v ? filled(v.value) : false,
      });
    } else if (n.kind === "secret" && n.name) {
      out.push({ ...base, kind: "secret", key: n.name, done: secrets.has(n.name) });
    } else if (n.kind === "provider" && n.node_id) {
      const node = def.nodes.find((x) => x.id === n.node_id);
      const p = node?.params?.provider;
      out.push({
        ...base,
        kind: "provider",
        key: n.node_id,
        done: !node || (typeof p === "string" && p !== "default_draft" && p !== ""),
      });
    } else {
      out.push({ ...base, kind: "app", key: n.name ?? n.kind, done: false });
    }
  });
  return out;
}

/** Set the value of a variable of the draft. A variable the draft lacks is added. */
export function setVariableValue(def: WfDef, key: string, value: unknown, kind?: string): WfDef {
  const vars = def.variables ?? [];
  if (vars.some((v) => v.key === key))
    return { ...def, variables: vars.map((v) => (v.key === key ? { ...v, value } : v)) };
  return {
    ...def,
    variables: [...vars, { key, kind: (kind ?? "text") as WfVariable["kind"], value }],
  };
}

export function setNodeProvider(def: WfDef, nodeId: string, provider: string): WfDef {
  return {
    ...def,
    nodes: def.nodes.map((n) =>
      n.id === nodeId ? { ...n, params: { ...(n.params ?? {}), provider } } : n,
    ),
  };
}

// ---------- paste a whole workflow ----------

function parseLoose(text: string): unknown {
  return JSON.parse(text.trim().replace(/^```(?:json)?\s*|\s*```$/g, ""));
}

/**
 * Text from the clipboard that is a whole workflow file (not the node clipboard of the canvas, which has only `nodes` and `edges`).
 * It has `nodes` and at least one of format, name, version, variables, settings, meta, or n8n's `connections`.
 */
export function isWholeWorkflow(text: string): boolean {
  const t = text.trim();
  if (t.length < 20 || (!t.startsWith("{") && !t.startsWith("`"))) return false;
  let v: unknown;
  try {
    v = parseLoose(t);
  } catch {
    return false;
  }
  if (!v || typeof v !== "object") return false;
  const o = (
    (v as Obj).workflow && typeof (v as Obj).workflow === "object" ? (v as Obj).workflow : v
  ) as Obj;
  if (!Array.isArray(o.nodes)) return false;
  return ["format", "name", "version", "variables", "settings", "meta", "connections"].some(
    (k) => k in o,
  );
}

export interface Added {
  def: WfDef;
  /** The ids of the new nodes on the canvas. */
  ids: string[];
  /** Nodes that got a new id because theirs was taken: `[from, to]`. */
  renamed: [string, string][];
}

/**
 * Add the nodes, notes, pins and missing variables of `incoming` to `def`.
 * A node whose id is taken gets a new id; expressions that read it and the edges follow.
 * The new nodes are put to the right of what is there.
 */
export function addWorkflowNodes(def: WfDef, incoming: WfDef): Added {
  const nodes = incoming.nodes ?? [];
  const keep = new Set(nodes.map((n) => n.id));
  const edges = (incoming.edges ?? []).filter((e) => keep.has(e.from) && keep.has(e.to));
  const here = def.nodes
    .map((n) => n.position?.x)
    .filter((x): x is number => typeof x === "number");
  const there = nodes.map((n) => n.position?.x).filter((x): x is number => typeof x === "number");
  const dx = here.length && there.length ? Math.max(...here) + 300 - Math.min(...there) : 0;
  const r = pasteFragment(def, { nodes, edges }, { x: dx, y: 0 });
  const renamed: [string, string][] = [];
  const map = new Map<string, string>();
  nodes.forEach((n, i) => {
    const to = r.ids[i] as string;
    map.set(n.id, to);
    if (to !== n.id) renamed.push([n.id, to]);
  });

  let next = r.def;
  const pins = incoming.pins ?? {};
  const movedPins = Object.entries(pins).filter(([k]) => map.has(k));
  if (movedPins.length)
    next = {
      ...next,
      pins: {
        ...(next.pins ?? {}),
        ...Object.fromEntries(movedPins.map(([k, v]) => [map.get(k) as string, v])),
      },
    };

  const noteIds = [...(next.notes ?? []).map((n) => n.id)];
  const notes: WfSticky[] = [];
  for (const n of incoming.notes ?? []) {
    const id = uniqueId(n.id, noteIds);
    noteIds.push(id);
    notes.push({ ...n, id, position: { x: n.position.x + dx, y: n.position.y } });
  }
  if (notes.length) next = { ...next, notes: [...(next.notes ?? []), ...notes] };

  const have = new Set((next.variables ?? []).map((v) => v.key));
  const fresh = (incoming.variables ?? []).filter((v) => !have.has(v.key));
  if (fresh.length) next = { ...next, variables: [...(next.variables ?? []), ...fresh] };
  return { def: next, ids: r.ids, renamed };
}

/** Words for the renames, for the toast. */
export function renameNote(renamed: [string, string][]): string {
  if (renamed.length === 0) return "";
  return `Renamed ${renamed.map(([a, b]) => `${a} to ${b}`).join(", ")} because the ids were taken.`;
}

export function nodeCount(def: { nodes?: WfNode[] }): number {
  return def.nodes?.length ?? 0;
}

// ---------- errors ----------

export interface JsonPlace {
  line: number;
  column: number;
}

/** Line and column of an `invalid_json` answer, or null for any other error. */
export function jsonPlace(e: unknown): JsonPlace | null {
  const p = e as Partial<ApiProblem> | null;
  if (p?.code !== "invalid_json") return null;
  const line = Number(p.details?.line);
  const column = Number(p.details?.column);
  return Number.isFinite(line) && line > 0
    ? { line, column: Number.isFinite(column) && column > 0 ? column : 1 }
    : null;
}

/** The line of the text where the JSON broke, and a caret under the column. Long lines are cut around the column. */
export function excerpt(text: string, place: JsonPlace): { line: string; caret: string } | null {
  const line = text.split(/\r?\n/)[place.line - 1];
  if (line === undefined) return null;
  const col = Math.min(place.column, line.length + 1);
  const start = Math.max(0, col - 40);
  const shown = line.slice(start, start + 80);
  return {
    line: (start > 0 ? "…" : "") + shown,
    caret: `${" ".repeat(col - 1 - start + (start > 0 ? 1 : 0))}^`,
  };
}

export function issueText(i: Issue): string {
  return i.node_id ? `${i.node_id}: ${i.message}` : i.message;
}

// ---------- download ----------

/** The file name from a `Content-Disposition` header, else a slug of the workflow name. */
export function fileNameFrom(header: string | null, name: string): string {
  const m = header?.match(/filename\*?=(?:UTF-8'')?"?([^";]+)"?/i);
  if (m?.[1]) {
    try {
      return decodeURIComponent(m[1]);
    } catch {
      return m[1];
    }
  }
  const slug =
    name
      .toLowerCase()
      .replace(/[^a-z0-9]+/g, "-")
      .replace(/^-|-$/g, "") || "workflow";
  return `${slug}.aikido-workflow.json`;
}

export function saveBlob(name: string, text: string, type: string): void {
  const url = URL.createObjectURL(new Blob([text], { type }));
  const a = document.createElement("a");
  a.href = url;
  a.download = name;
  a.click();
  URL.revokeObjectURL(url);
}
