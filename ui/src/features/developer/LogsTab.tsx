import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http } from "@/api/http";
import { cn } from "@/lib/cn";
import { copyText, type LogEntry, type LogsReply, logLine } from "./api";

const KEEP = 3000;
const LEVELS = ["DEBUG", "INFO", "WARN", "ERROR"] as const;
const RANK: Record<string, number> = { TRACE: 0, DEBUG: 1, INFO: 2, WARN: 3, ERROR: 4 };
const COLOR: Record<string, string> = {
  ERROR: "bg-danger/15 text-danger",
  WARN: "bg-warning/15 text-warning",
  INFO: "bg-info/15 text-info",
  DEBUG: "bg-surface-2 text-muted",
  TRACE: "bg-surface-2 text-muted",
};
const FILTERS: [string, string][] = [
  ["info,aikido_email=debug,sqlx=warn,hyper=warn,rustls=warn,hickory_proto=warn,hickory_resolver=warn,tower_http=warn", "Normal: Aikido at debug"],
  ["info,sqlx=warn", "Quiet: info only"],
  ["info,aikido_email=trace,sqlx=warn", "Detailed: Aikido at trace"],
  ["info,aikido_email=debug,sqlx=debug", "With every SQL query"],
];

const time = (iso: string) => {
  const d = new Date(iso);
  return `${d.toLocaleTimeString([], { hour12: false })}.${String(d.getMilliseconds()).padStart(3, "0")}`;
};

/** The live log. Polls the server every 1.5 seconds for what is new. */
export function LogsTab({ onCounts }: { onCounts: (errors: number, warns: number) => void }) {
  const [entries, setEntries] = useState<LogEntry[]>([]);
  const [paused, setPaused] = useState(false);
  const [follow, setFollow] = useState(true);
  const [level, setLevel] = useState<(typeof LEVELS)[number]>("INFO");
  const [text, setText] = useState("");
  const [filter, setFilter] = useState("");
  const [open, setOpen] = useState<number | null>(null);
  const lastId = useRef(0);
  const box = useRef<HTMLDivElement>(null);

  const poll = useCallback(async () => {
    try {
      const r = await http.get<LogsReply>("/v1/dev/logs", {
        query: { after: lastId.current || undefined, limit: 1000 },
      });
      setFilter((f) => (f === r.filter ? f : r.filter));
      // a smaller id than we know: the server restarted or the log was cleared
      if (r.last_id < lastId.current) {
        lastId.current = 0;
        setEntries([]);
        return;
      }
      lastId.current = r.last_id;
      if (r.entries.length > 0) setEntries((old) => [...old, ...r.entries].slice(-KEEP));
    } catch {
      // the server may be restarting: try again on the next round
    }
  }, []);

  useEffect(() => {
    void poll();
    if (paused) return;
    const t = setInterval(() => void poll(), 1500);
    return () => clearInterval(t);
  }, [poll, paused]);

  useEffect(() => {
    onCounts(
      entries.filter((e) => e.level === "ERROR").length,
      entries.filter((e) => e.level === "WARN").length,
    );
  }, [entries, onCounts]);

  const shown = useMemo(() => {
    const q = text.trim().toLowerCase();
    return entries.filter(
      (e) =>
        (RANK[e.level] ?? 0) >= (RANK[level] ?? 0) &&
        (!q ||
          `${e.message} ${e.fields} ${e.target} ${e.span ?? ""}`.toLowerCase().includes(q)),
    );
  }, [entries, level, text]);

  // biome-ignore lint/correctness/useExhaustiveDependencies: scroll down whenever the shown lines change
  useEffect(() => {
    if (follow && box.current) box.current.scrollTop = box.current.scrollHeight;
  }, [shown, follow]);

  async function clear() {
    try {
      await http.del("/v1/dev/logs");
      setEntries([]);
    } catch (e) {
      toast.error(messageOf(e));
    }
  }
  async function changeFilter(f: string) {
    try {
      await http.put("/v1/dev/log-filter", { body: { filter: f } });
      setFilter(f);
      toast.success("Log detail changed.");
    } catch (e) {
      toast.error(messageOf(e));
    }
  }

  const known = FILTERS.some(([f]) => f === filter);
  return (
    <div className="flex h-full min-h-0 flex-col">
      <div className="flex flex-wrap items-center gap-2 border-b border-border px-3 py-1.5 text-xs">
        <label className="flex items-center gap-1">
          Level
          <select
            aria-label="Lowest level to show"
            className="h-7 rounded border border-border bg-surface-2 px-1"
            value={level}
            onChange={(e) => setLevel(e.target.value as (typeof LEVELS)[number])}
          >
            {LEVELS.map((l) => (
              <option key={l} value={l}>
                {l === "DEBUG" ? "All" : `${l[0]}${l.slice(1).toLowerCase()} and up`}
              </option>
            ))}
          </select>
        </label>
        <input
          aria-label="Search the log"
          placeholder="Search"
          className="h-7 w-44 rounded border border-border bg-surface-2 px-2"
          value={text}
          onChange={(e) => setText(e.target.value)}
        />
        <button type="button" className={cn(BTN, level === "WARN" && "bg-accent-soft")} onClick={() => setLevel(level === "WARN" ? "INFO" : "WARN")}>
          Problems only
        </button>
        <button type="button" className={BTN} onClick={() => setPaused(!paused)}>
          {paused ? "Resume" : "Pause"}
        </button>
        <label className="flex items-center gap-1">
          <input type="checkbox" checked={follow} onChange={(e) => setFollow(e.target.checked)} /> Follow
        </label>
        <button type="button" className={BTN} onClick={() => void copyText(shown.map(logLine).join("\n"))}>
          Copy shown ({shown.length})
        </button>
        <button type="button" className={BTN} onClick={() => void clear()}>
          Clear
        </button>
        <label className="ml-auto flex items-center gap-1">
          Detail
          <select
            aria-label="How much the server writes to this log"
            className="h-7 max-w-52 rounded border border-border bg-surface-2 px-1"
            value={known ? filter : "custom"}
            onChange={(e) => e.target.value !== "custom" && void changeFilter(e.target.value)}
          >
            {FILTERS.map(([f, label]) => (
              <option key={f} value={f}>
                {label}
              </option>
            ))}
            {!known && <option value="custom">Custom: {filter}</option>}
          </select>
        </label>
      </div>
      <div ref={box} className="min-h-0 flex-1 overflow-auto font-mono text-xs leading-5">
        {shown.length === 0 && (
          <p className="p-3 text-muted">
            Nothing to show yet. The log fills as the server works. It is kept in memory (the last 5000 lines) and is empty after a restart.
          </p>
        )}
        {shown.map((e) => (
          <div
            key={e.id}
            className={cn(
              "group border-b border-border/40 px-3 py-0.5 hover:bg-surface-2",
              e.level === "ERROR" && "bg-danger/5",
            )}
          >
            <button
              type="button"
              className="flex w-full items-baseline gap-2 text-left"
              onClick={() => setOpen(open === e.id ? null : e.id)}
            >
              <span className="shrink-0 text-muted">{time(e.at)}</span>
              <span className={cn("w-12 shrink-0 rounded px-1 text-center", COLOR[e.level])}>{e.level}</span>
              <span className="hidden shrink-0 text-muted md:inline">{e.target.replace("aikido_email::", "")}</span>
              <span className="min-w-0 flex-1 break-words">
                {e.message}
                {e.fields && <span className="text-muted"> {e.fields}</span>}
              </span>
              <span
                role="button"
                tabIndex={0}
                aria-label="Copy this line"
                className="shrink-0 rounded px-1 text-muted opacity-0 hover:bg-surface group-hover:opacity-100"
                onClick={(ev) => {
                  ev.stopPropagation();
                  void copyText(logLine(e));
                }}
                onKeyDown={(ev) => ev.key === "Enter" && void copyText(logLine(e))}
              >
                Copy
              </span>
            </button>
            {open === e.id && (
              <pre className="mt-1 whitespace-pre-wrap break-words rounded bg-surface-2 p-2 text-muted">
                {[`time    ${e.at}`, `target  ${e.target}`, e.span ? `request ${e.span}` : "", `message ${e.message}`, e.fields ? `fields  ${e.fields}` : ""]
                  .filter(Boolean)
                  .join("\n")}
              </pre>
            )}
          </div>
        ))}
      </div>
    </div>
  );
}

const BTN = "h-7 rounded border border-border bg-surface-2 px-2 hover:bg-accent-soft";
