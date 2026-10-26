import { createContext, useContext } from "react";
import type { Issue, NodeSpec, RunDetail, WfDef } from "./types";

export type Leveled = Issue & { level: "error" | "warning" };

/** What every part of the editor needs to know. */
export interface EditorCtx {
  workflowId: string;
  def: WfDef;
  specs: Map<string, NodeSpec>;
  /** Errors and warnings of the draft, by node id. */
  issues: Map<string, Leveled[]>;
  /** The run drawn on the canvas (live, or one opened from Executions). */
  run: RunDetail | null;
  /** The run that data comes from: the one on the canvas, else the newest test run. */
  dataRun: RunDetail | null;
  live: boolean;
  readOnly: boolean;
  /** Goes up after every good save, so previews read the saved draft. */
  saveTick: number;
  /** Node ids that flash once (the AI just added or changed them). */
  flash: string[];
  isAdmin: boolean;
}

const Ctx = createContext<EditorCtx | null>(null);
export const EditorProvider = Ctx.Provider;

export function useEditor(): EditorCtx {
  const c = useContext(Ctx);
  if (!c) throw new Error("useEditor must be used inside the editor");
  return c;
}

/** Group the issues of a validation by node. Issues of the whole file go under `""`. */
export function groupIssues(errors: Issue[], warnings: Issue[]): Map<string, Leveled[]> {
  const map = new Map<string, Leveled[]>();
  const add = (i: Issue, level: Leveled["level"]) => {
    const key = i.node_id ?? "";
    map.set(key, [...(map.get(key) ?? []), { ...i, level }]);
  };
  for (const e of errors) add(e, "error");
  for (const w of warnings) add(w, "warning");
  return map;
}
