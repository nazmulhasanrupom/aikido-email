import { useCallback, useEffect, useRef, useState } from "react";
import { call } from "./api";
import { FINISHED, type RunDetail, type RunEvent } from "./types";

const POLL_MS = 1000;

export interface LiveRun {
  runId: string | null;
  run: RunDetail | null;
  events: RunEvent[];
  /** Still waiting for the run to end. */
  live: boolean;
  error: string | null;
}

const EMPTY: LiveRun = { runId: null, run: null, events: [], live: false, error: null };

/**
 * One run that the editor shows: a test run that was just started, or a run picked from the Executions tab.
 * While it is not finished, the events are read every second and the run page is read again when something happened.
 */
export function useLiveRun() {
  const [state, setState] = useState<LiveRun>(EMPTY);
  const token = useRef(0);

  const watch = useCallback(async (runId: string) => {
    const mine = ++token.current;
    setState({ runId, run: null, events: [], live: true, error: null });
    let after = 0;
    let first = true;
    let failures = 0;
    for (;;) {
      if (token.current !== mine) return;
      try {
        const ev = await call.events(runId, after);
        if (token.current !== mine) return;
        failures = 0;
        const fresh = ev.events;
        if (fresh.length) after = fresh[fresh.length - 1]?.id ?? after;
        const ended = FINISHED.includes(ev.status);
        if (fresh.length || first || ended) {
          const run = await call.runDetail(runId);
          if (token.current !== mine) return;
          setState((s) => ({
            ...s,
            run,
            events: [...s.events, ...fresh],
            live: !ended && !FINISHED.includes(run.status),
          }));
          first = false;
          if (ended || FINISHED.includes(run.status)) return;
        }
      } catch {
        failures += 1;
        if (failures >= 5) {
          setState((s) => ({ ...s, live: false, error: "Lost the connection to the run." }));
          return;
        }
      }
      await new Promise((r) => setTimeout(r, POLL_MS));
    }
  }, []);

  /** Show a finished run (no polling). */
  const show = useCallback(async (runId: string) => {
    const mine = ++token.current;
    setState({ runId, run: null, events: [], live: false, error: null });
    try {
      const run = await call.runDetail(runId);
      if (token.current !== mine) return;
      setState({ runId, run, events: [], live: false, error: null });
      if (!FINISHED.includes(run.status)) void watch(runId);
    } catch {
      if (token.current === mine) setState((s) => ({ ...s, error: "Could not load this run." }));
    }
  }, [watch]);

  const clear = useCallback(() => {
    token.current += 1;
    setState(EMPTY);
  }, []);

  useEffect(
    () => () => {
      token.current += 1;
    },
    [],
  );

  return { ...state, watch, show, clear };
}

/** The newest row of a node in a run (a node can run more than once). */
export function nodeRow(run: RunDetail | null, nodeId: string) {
  if (!run) return undefined;
  let best: RunDetail["nodes"][number] | undefined;
  for (const n of run.nodes) if (n.node_id === nodeId && (!best || n.seq > best.seq)) best = n;
  return best;
}
