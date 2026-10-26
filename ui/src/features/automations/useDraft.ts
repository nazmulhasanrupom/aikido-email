import { useCallback, useEffect, useRef, useState } from "react";
import { ApiProblem } from "@/api/errors";
import { call } from "./api";
import type { Validation, WfDef, WorkflowDetail } from "./types";

export type SaveState = "saved" | "dirty" | "saving" | "offline" | "conflict";

interface Conflict {
  draft: WfDef;
  hash: string;
}

const HISTORY = 100;
const AUTOSAVE_MS = 1000;
const RETRY_MS = 5000;

/**
 * The draft of one workflow while it is edited: the working copy, an undo list, and the autosave
 * (one second after the last change, with the hash we last saw so a second tab cannot be overwritten by mistake).
 * If the connection is lost, the copy stays in memory and the save is tried again.
 */
export function useDraft(initial: WorkflowDetail) {
  const id = initial.id;
  const [def, setDef] = useState<WfDef>(initial.draft);
  const [validation, setValidation] = useState<Validation>(initial.validation);
  const [state, setState] = useState<SaveState>("saved");
  const [conflict, setConflict] = useState<Conflict | null>(null);
  const [counts, setCounts] = useState({ undo: 0, redo: 0 });

  const latest = useRef(def);
  const hash = useRef(initial.draft_hash);
  const saving = useRef(false);
  const dirty = useRef(false);
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);
  const undoStack = useRef<WfDef[]>([]);
  const redoStack = useRef<WfDef[]>([]);
  const blocked = useRef(false);
  const lastKey = useRef<string | undefined>(undefined);
  const lastAt = useRef(0);
  const onSaved = useRef<(w: WorkflowDetail) => void>(() => {});

  const sync = () => setCounts({ undo: undoStack.current.length, redo: redoStack.current.length });

  const save = useCallback(async () => {
    if (saving.current || blocked.current || !dirty.current) return;
    saving.current = true;
    dirty.current = false;
    setState("saving");
    const sent = latest.current;
    try {
      const res = await call.patch(id, { draft: sent, if_hash: hash.current, name: sent.name });
      hash.current = res.draft_hash;
      setValidation(res.validation);
      onSaved.current(res);
      saving.current = false;
      if (dirty.current) {
        setState("dirty");
        void save();
      } else setState("saved");
    } catch (e) {
      saving.current = false;
      dirty.current = true;
      if (e instanceof ApiProblem && e.code === "draft_conflict") {
        const d = e.details as { draft?: WfDef; draft_hash?: string };
        blocked.current = true;
        setConflict({ draft: d.draft as WfDef, hash: String(d.draft_hash ?? "") });
        setState("conflict");
      } else if (e instanceof ApiProblem && e.status < 500 && e.status !== 429) {
        // the server refuses this draft (too large, bad name...). Keep it and say so.
        setState("offline");
        timer.current = setTimeout(() => void save(), RETRY_MS * 3);
      } else {
        setState("offline");
        timer.current = setTimeout(() => void save(), RETRY_MS);
      }
    }
  }, [id]);

  const schedule = useCallback(() => {
    dirty.current = true;
    if (!blocked.current) setState((s) => (s === "saving" ? s : "dirty"));
    clearTimeout(timer.current);
    timer.current = setTimeout(() => void save(), AUTOSAVE_MS);
  }, [save]);

  /** Change the draft. `record: false` for steps that are not worth an undo (a drag in progress). */
  const update = useCallback(
    (fn: (d: WfDef) => WfDef, opts: { record?: boolean; key?: string } = {}) => {
      const prev = latest.current;
      const next = fn(prev);
      if (next === prev) return;
      if (opts.record !== false) {
        // typing in one field is one undo step, not one per letter
        const now = Date.now();
        const joined = opts.key !== undefined && opts.key === lastKey.current && now - lastAt.current < 1500;
        lastKey.current = opts.key;
        lastAt.current = now;
        if (!joined) {
          undoStack.current.push(prev);
          if (undoStack.current.length > HISTORY) undoStack.current.shift();
          redoStack.current = [];
          sync();
        }
      }
      latest.current = next;
      setDef(next);
      schedule();
    },
    [schedule],
  );

  /** Remember the present draft for undo (call before a drag starts). */
  const mark = useCallback(() => {
    undoStack.current.push(latest.current);
    if (undoStack.current.length > HISTORY) undoStack.current.shift();
    redoStack.current = [];
    sync();
  }, []);

  const undo = useCallback(() => {
    const prev = undoStack.current.pop();
    if (!prev) return;
    redoStack.current.push(latest.current);
    latest.current = prev;
    setDef(prev);
    sync();
    schedule();
  }, [schedule]);

  const redo = useCallback(() => {
    const next = redoStack.current.pop();
    if (!next) return;
    undoStack.current.push(latest.current);
    latest.current = next;
    setDef(next);
    sync();
    schedule();
  }, [schedule]);

  const saveNow = useCallback(() => {
    clearTimeout(timer.current);
    void save();
  }, [save]);

  /** Save what is waiting and tell if the server has the newest draft now (a test run reads it from there). */
  const flush = useCallback(async (): Promise<boolean> => {
    clearTimeout(timer.current);
    for (let i = 0; i < 100; i++) {
      if (blocked.current) return false;
      if (!dirty.current && !saving.current) return true;
      if (!saving.current) await save();
      else await new Promise((r) => setTimeout(r, 100));
      if (!saving.current && dirty.current && !blocked.current) {
        // the save failed (offline): do not wait for ever
        await new Promise((r) => setTimeout(r, 200));
        if (i > 3) return false;
      }
    }
    return false;
  }, [save]);

  /** Take a draft that the server gave (restore of a version, the other tab's draft). */
  const adopt = useCallback((w: { draft: WfDef; draft_hash: string; validation?: Validation }) => {
    clearTimeout(timer.current);
    latest.current = w.draft;
    hash.current = w.draft_hash;
    dirty.current = false;
    blocked.current = false;
    undoStack.current = [];
    redoStack.current = [];
    sync();
    setDef(w.draft);
    if (w.validation) setValidation(w.validation);
    setConflict(null);
    setState("saved");
  }, []);

  const useTheirs = useCallback(() => {
    if (conflict) adopt({ draft: conflict.draft, draft_hash: conflict.hash });
  }, [conflict, adopt]);

  const overwrite = useCallback(() => {
    if (!conflict) return;
    hash.current = conflict.hash;
    blocked.current = false;
    setConflict(null);
    dirty.current = true;
    void save();
  }, [conflict, save]);

  // leaving the page with unsaved work: try once more, and ask the browser to warn
  useEffect(() => {
    const warn = (e: BeforeUnloadEvent) => {
      if (dirty.current || saving.current) e.preventDefault();
    };
    window.addEventListener("beforeunload", warn);
    return () => {
      window.removeEventListener("beforeunload", warn);
      clearTimeout(timer.current);
      if (dirty.current && !blocked.current && !saving.current) {
        void call.patch(id, { draft: latest.current, if_hash: hash.current, name: latest.current.name }).catch(() => {});
      }
    };
  }, [id]);

  return {
    def,
    update,
    mark,
    undo,
    redo,
    canUndo: counts.undo > 0,
    canRedo: counts.redo > 0,
    saveNow,
    flush,
    state,
    validation,
    conflict,
    useTheirs,
    overwrite,
    adopt,
    /** Called with the answer of every good save (the page uses it to refresh the header). */
    onSaved: (fn: (w: WorkflowDetail) => void) => {
      onSaved.current = fn;
    },
  };
}
