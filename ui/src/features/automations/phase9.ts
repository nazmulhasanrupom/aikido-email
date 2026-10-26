/** Pure helpers of the Phase 9 widgets: code errors, sub-workflow pickers, agent tools, pagination. */
import { isTrigger } from "./defs";
import type { Obj, WfDef } from "./types";

// ---------- code.js ----------

export const CODE_MAX = 50000;

/** The line (1 based) that a code error names, like `The code failed: boom (line 3)`. */
export function codeErrorLine(
  issues: { code?: string; path?: string | null; message: string }[],
): number | null {
  for (const i of issues) {
    if (i.path && i.path !== "params.code") continue;
    const m = /\(line (\d+)\)/i.exec(i.message) ?? /\bline (\d+)/i.exec(i.message);
    if (m?.[1]) return Number(m[1]);
  }
  return null;
}

/** Insert `indent` at the selection (or at every selected line when it spans lines); shift+tab removes one level. */
export function indentSelection(
  text: string,
  start: number,
  end: number,
  back: boolean,
  indent = "  ",
): { text: string; start: number; end: number } {
  const multi = text.slice(start, end).includes("\n");
  if (!back && !multi) {
    return {
      text: text.slice(0, start) + indent + text.slice(end),
      start: start + indent.length,
      end: start + indent.length,
    };
  }
  const from = text.lastIndexOf("\n", start - 1) + 1;
  const lines = text.slice(from, end).split("\n");
  let first = 0;
  let total = 0;
  const out = lines.map((l, i) => {
    let next = l;
    if (back) {
      const m = new RegExp(`^( {1,${indent.length}}|\\t)`).exec(l);
      next = m ? l.slice(m[0].length) : l;
    } else {
      next = indent + l;
    }
    const delta = next.length - l.length;
    if (i === 0) first = delta;
    total += delta;
    return next;
  });
  return {
    text: text.slice(0, from) + out.join("\n") + text.slice(end),
    start: Math.max(from, start + first),
    end: end + total,
  };
}

// ---------- flow.execute_workflow ----------

/** The trigger nodes of a workflow draft: what `trigger` can name. */
export function triggerNodes(def: WfDef): { id: string; label: string }[] {
  return def.nodes
    .filter((n) => isTrigger(n.type))
    .map((n) => ({ id: n.id, label: n.name || n.id }));
}

/** The nodes of a workflow draft whose items can come back (`return_node`). */
export function returnNodes(def: WfDef): { id: string; label: string }[] {
  return def.nodes
    .filter((n) => !isTrigger(n.type))
    .map((n) => ({ id: n.id, label: n.name || n.id }));
}

// ---------- ai.agent tools ----------

export const TOOL_NAME = /^[a-z][a-z0-9_]{0,39}$/;
export const MAX_TOOLS = 10;
const BLOCKED_TOOL_NODES = new Set(["aikido.api", "aikido.campaign.status"]);

/** A node type an agent may call as a tool. */
export function allowedToolNode(type: string): boolean {
  if (BLOCKED_TOOL_NODES.has(type)) return false;
  return type.startsWith("aikido.") || type === "http.request" || type === "web.fetch_page";
}

export interface ToolDraft {
  name?: string;
  description?: string;
  node?: string;
  params?: Obj;
  args?: unknown[];
}

export interface ToolProblem {
  index: number;
  field: "name" | "description" | "node";
  message: string;
}

/** Problems of the tool list the validator would also find: name format, unique names, description length, node. */
export function toolProblems(tools: ToolDraft[]): ToolProblem[] {
  const out: ToolProblem[] = [];
  const seen = new Map<string, number>();
  tools.forEach((t, index) => {
    const name = t.name ?? "";
    if (!TOOL_NAME.test(name)) {
      out.push({
        index,
        field: "name",
        message: "Use a-z, 0-9 and _, start with a letter, at most 40 characters.",
      });
    } else if (seen.has(name)) {
      out.push({
        index,
        field: "name",
        message: `The name is also used by tool ${(seen.get(name) ?? 0) + 1}.`,
      });
    } else {
      seen.set(name, index);
    }
    const d = (t.description ?? "").trim();
    if (d.length < 1 || d.length > 500)
      out.push({
        index,
        field: "description",
        message: "Describe the tool in 1 to 500 characters.",
      });
    if (!t.node || !allowedToolNode(t.node))
      out.push({ index, field: "node", message: "Choose a node the AI may call." });
  });
  return out;
}

/** A name for a new tool that is free: `tool_1`, `tool_2`, ... */
export function freeToolName(tools: ToolDraft[]): string {
  const used = new Set(tools.map((t) => t.name));
  for (let i = 1; i <= MAX_TOOLS + 1; i++) if (!used.has(`tool_${i}`)) return `tool_${i}`;
  return "tool";
}

// ---------- http.request pagination ----------

export type PageMode = "next_url" | "page" | "offset" | "cursor";
export const PAGE_MODES: PageMode[] = ["next_url", "page", "offset", "cursor"];

const COMMON = ["stop_when", "max_pages", "delay_ms"];
const BY_MODE: Record<PageMode, string[]> = {
  next_url: ["next", ...COMMON],
  page: ["param", "location", "start", ...COMMON],
  offset: ["param", "location", "start", "step", ...COMMON],
  cursor: ["next", "param", "location", ...COMMON],
};

/** The fields of the pagination block that matter for a mode, in order. */
export function paginationFields(mode: string | undefined): string[] {
  return BY_MODE[(mode ?? "next_url") as PageMode] ?? BY_MODE.next_url;
}

/** The block with the fields of other modes removed (after the mode changed). */
export function pruneForMode(pg: Obj, mode: PageMode): Obj {
  const keep = new Set(["mode", ...paginationFields(mode)]);
  return Object.fromEntries(Object.entries({ ...pg, mode }).filter(([k]) => keep.has(k)));
}

/** What the validator says about the block, found early. `hasSplit`: the node has a `split` path. */
export function paginationProblems(pg: Obj, hasSplit: boolean): string[] {
  const out: string[] = [];
  const mode = pg.mode as string | undefined;
  const has = (k: string) =>
    typeof pg[k] === "string"
      ? (pg[k] as string).trim() !== ""
      : pg[k] !== undefined && pg[k] !== null;
  if ((mode === "page" || mode === "offset" || mode === "cursor") && !has("param"))
    out.push("Name the page, offset or cursor param.");
  if ((mode === "page" || mode === "offset") && !hasSplit && !has("stop_when"))
    out.push(
      "Set `split` on the node or a stop condition, or the loop would never know it is done.",
    );
  if (mode === "offset" && !has("step") && !hasSplit)
    out.push(
      "Set a step, or `split` on the node (the step is then the item count of the last page).",
    );
  return out;
}

export const PAGE_ROOT = [{ text: "page", hint: "the page just read", more: true }] as const;

/** A sample of `page` for the autocomplete of the stop condition. */
export const PAGE_SAMPLE: Obj = {
  page: { number: 1, status: 200, headers: {}, body: {}, url: "", items: [], count: 0 },
};
