import { ApiProblem } from "@/api/errors";

/** Pure logic of the AI box (map 16): chip text, error words, the "Try to fix" text. */

export const MAX_MESSAGE = 8000;

export interface Thread {
  id: string;
  workflow_id: string | null;
  title: string;
  message_count: number;
  created_at: string;
  updated_at: string;
}

export interface CopilotIssue {
  code: string;
  node_id?: string;
  path?: string;
  message: string;
}

export interface Diff {
  added: string[];
  removed: string[];
  changed: string[];
  edges_added: number;
  edges_removed: number;
  fields_changed?: unknown;
}

export interface AppliedOk {
  workflow_id: string;
  mode: "create" | "replace" | "proposed";
  diff: Diff;
  draft_history_id?: string | number;
  undone?: boolean;
}

export interface AppliedFail {
  error: string;
  error_code: "invalid_workflow" | "bad_answer";
  errors: CopilotIssue[];
}

export type Applied = AppliedOk | AppliedFail;

export interface ChatMessage {
  id: number;
  role: "user" | "assistant";
  content: string;
  applied: Applied | null;
  has_workflow: boolean;
  created_at: string;
}

export function isFail(a: Applied | null | undefined): a is AppliedFail {
  return !!a && "error_code" in a;
}

export function isOk(a: Applied | null | undefined): a is AppliedOk {
  return !!a && !("error_code" in a) && "diff" in a;
}

const plural = (n: number, one: string, many = `${one}s`) => `${n} ${n === 1 ? one : many}`;

/** The text of the diff: "3 nodes added, 1 changed, 1 removed". Empty parts are left out. */
export function diffText(d: Diff): string {
  const parts: string[] = [];
  if (d.added.length) parts.push(`${plural(d.added.length, "node")} added`);
  if (d.changed.length) parts.push(`${d.changed.length} changed`);
  if (d.removed.length) parts.push(`${d.removed.length} removed`);
  if (d.edges_added) parts.push(`${plural(d.edges_added, "connection")} added`);
  if (d.edges_removed) parts.push(`${plural(d.edges_removed, "connection")} removed`);
  return parts.join(", ");
}

export type Chip =
  | { kind: "applied"; text: string; canUndo: boolean; undone: boolean }
  | { kind: "proposed"; text: string }
  | { kind: "failed"; text: string; errors: CopilotIssue[] };

/** What the chip under an assistant message says. `null`: the AI only answered, no chip. */
export function chipOf(applied: Applied | null | undefined): Chip | null {
  if (!applied) return null;
  if (isFail(applied)) {
    const n = applied.errors?.length ?? 0;
    return {
      kind: "failed",
      text: n > 0 ? `Could not apply: ${plural(n, "error")}` : "Could not apply",
      errors: applied.errors ?? [],
    };
  }
  const t = diffText(applied.diff) || "no changes";
  if (applied.mode === "proposed") return { kind: "proposed", text: `Proposed: ${t}` };
  if (applied.undone)
    return { kind: "applied", text: `Applied, then undone: ${t}`, canUndo: false, undone: true };
  return {
    kind: "applied",
    text: `Applied: ${t}`,
    canUndo: applied.mode === "replace" && applied.draft_history_id !== undefined,
    undone: false,
  };
}

/** Nodes that flash on the canvas after a change. */
export function flashIds(applied: Applied | null | undefined): string[] {
  return isOk(applied) ? [...applied.diff.added, ...applied.diff.changed] : [];
}

/** The "Try to fix" message: the errors, one per line. */
export function fixText(errors: CopilotIssue[]): string {
  const lines = errors.slice(0, 20).map((e) => {
    const where = [e.node_id, e.path].filter(Boolean).join(" ");
    return `- ${e.code}${where ? ` (${where})` : ""}: ${e.message}`;
  });
  const more = errors.length > 20 ? `\n- and ${errors.length - 20} more` : "";
  return `Please fix these errors in the workflow:\n${lines.join("\n")}${more}`;
}

export interface Shown {
  text: string;
  /** A link to the place that fixes it. */
  link?: "ai-settings";
}

const WORDS: Record<string, string> = {
  empty_message: "Write a message first.",
  message_too_long: `The message is too long. Keep it under ${MAX_MESSAGE} characters.`,
  draft_too_large: "The workflow is too big for the AI. Remove some nodes, then try again.",
  ai_not_configured: "Set up an AI provider in Settings > AI.",
  ai_provider_not_found:
    "The AI provider chosen for the AI box does not exist any more. Pick another in Settings > AI.",
  ai_provider_auth_failed: "The AI provider refused the key. Check the key in Settings > AI.",
  daily_budget_reached: "The daily AI budget is used up. Try again tomorrow, or raise the budget.",
  copilot_disabled: "The AI box is turned off in the automation settings.",
  thread_workflow_mismatch: "This chat belongs to another workflow. Start a new chat.",
  thread_not_found: "This chat does not exist any more. Start a new chat.",
  workflow_not_found: "This workflow does not exist any more.",
  ai_provider_unavailable: "The AI provider did not answer. Try again in a moment.",
  ai_bad_request: "The AI provider did not accept the request. Try a shorter message.",
  ai_refused: "The AI refused to answer this. Say it in other words.",
  ai_bad_response: "The AI answered in a form that could not be read. Try again.",
};

/** The words for a failed send or undo. */
export function errorShown(e: unknown): Shown {
  if (e instanceof ApiProblem) {
    if (e.code === "rate_limited") {
      const s = e.retryAfter;
      return {
        text: s
          ? `Too many messages. Try again in ${s} ${s === 1 ? "second" : "seconds"}.`
          : "Too many messages. Wait a little, then try again.",
      };
    }
    const w = WORDS[e.code];
    if (w) {
      const link =
        e.code === "ai_not_configured" ||
        e.code === "ai_provider_not_found" ||
        e.code === "ai_provider_auth_failed"
          ? ("ai-settings" as const)
          : undefined;
      return link ? { text: w, link } : { text: w };
    }
    return { text: e.message };
  }
  if (e instanceof TypeError) return { text: "The server did not answer. Check your connection." };
  return { text: "Something went wrong." };
}

/** The words for a failed undo. */
export function undoWords(e: unknown): string {
  if (e instanceof ApiProblem) {
    if (e.code === "nothing_to_undo") return "There is nothing to undo for this message.";
    if (e.code === "history_not_found")
      return "The old version is not kept any more, so it cannot be undone.";
    if (e.code === "message_not_found" || e.code === "workflow_not_found")
      return "The message or the workflow does not exist any more.";
  }
  return errorShown(e).text;
}

/** Can this text be sent? `over` is how far it is past the limit. */
export function sendCheck(text: string): { ok: boolean; over: number; empty: boolean } {
  const empty = text.trim().length === 0;
  const over = Math.max(0, text.length - MAX_MESSAGE);
  return { ok: !empty && over === 0, over, empty };
}

/** The title of a chat for the menu. */
export function threadTitle(t: Pick<Thread, "title">): string {
  return t.title.trim() || "New chat";
}

/** Add new messages to a list by id (no doubles), oldest first. */
export function mergeMessages(list: ChatMessage[], add: ChatMessage[]): ChatMessage[] {
  const map = new Map(list.map((m) => [m.id, m]));
  for (const m of add) map.set(m.id, m);
  return [...map.values()].sort((a, b) => a.id - b.id);
}

const KEY = "automations.openChat";
let memory: string | null = null;

/** Remember that the editor of this workflow should open with the chat open (after a workflow was made by the AI). */
export function wantChat(workflowId: string): void {
  memory = workflowId;
  try {
    sessionStorage.setItem(KEY, workflowId);
  } catch {
    /* storage can be blocked */
  }
}

/** True once for a workflow that `wantChat` named. */
export function takeWantChat(workflowId: string): boolean {
  let stored: string | null = memory;
  try {
    stored = sessionStorage.getItem(KEY) ?? stored;
    if (stored === workflowId) sessionStorage.removeItem(KEY);
  } catch {
    /* storage can be blocked */
  }
  if (stored !== workflowId) return false;
  memory = null;
  return true;
}
