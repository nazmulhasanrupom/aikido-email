import { useQuery } from "@tanstack/react-query";
import { useState } from "react";
import { messageOf } from "@/api/errors";
import { http } from "@/api/http";
import { cn } from "@/lib/cn";
import { readStored, writeStored } from "@/lib/safe-storage";
import { copyText, type SqlReply, tablesQuery } from "./api";

type Mode = "read" | "preview" | "commit";
const HISTORY = "aikido.dev.sql";
const BTN = "h-7 rounded border border-border bg-surface-2 px-2 text-xs hover:bg-accent-soft";

function history(): string[] {
  try {
    const v = JSON.parse(readStored(HISTORY) ?? "[]");
    return Array.isArray(v) ? v.filter((x) => typeof x === "string") : [];
  } catch {
    return [];
  }
}

const tsv = (r: SqlReply) =>
  [r.columns.join("\t"), ...r.rows.map((row) => row.map((c) => (c ?? "NULL").replace(/[\t\n]/g, " ")).join("\t"))].join("\n");
const json = (r: SqlReply) =>
  JSON.stringify(
    r.rows.map((row) => Object.fromEntries(r.columns.map((c, i) => [c, row[i]]))),
    null,
    2,
  );

/** A SQL console. Read only by default. Changes need a deliberate choice of mode. */
export function SqlTab() {
  const [sql, setSql] = useState("SELECT id, status, to_email, last_error FROM send_jobs ORDER BY created_at DESC LIMIT 50");
  const [mode, setMode] = useState<Mode>("read");
  const [busy, setBusy] = useState(false);
  const [result, setResult] = useState<SqlReply | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [past, setPast] = useState<string[]>(history);
  const tables = useQuery(tablesQuery);
  const [cols, setCols] = useState<{ table: string; list: { name: string; data_type: string; nullable: boolean }[] } | null>(null);

  async function run() {
    if (mode === "commit" && !window.confirm("Run this and SAVE the changes to the database? This cannot be undone.")) return;
    setBusy(true);
    setError(null);
    try {
      const r = await http.post<SqlReply>("/v1/dev/sql", { body: { sql, mode } });
      setResult(r);
      const next = [sql, ...past.filter((p) => p !== sql)].slice(0, 20);
      setPast(next);
      writeStored(HISTORY, JSON.stringify(next));
    } catch (e) {
      setResult(null);
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }
  async function showColumns(table: string) {
    setSql(`SELECT * FROM ${table} ORDER BY 1 DESC LIMIT 100`);
    try {
      const r = await http.get<{ columns: { name: string; data_type: string; nullable: boolean }[] }>(`/v1/dev/tables/${table}`);
      setCols({ table, list: r.columns });
    } catch {
      setCols(null);
    }
  }

  return (
    <div className="flex h-full min-h-0">
      <div className="hidden w-48 shrink-0 flex-col overflow-auto border-r border-border text-xs md:flex">
        <p className="px-3 py-1.5 font-medium text-muted">Tables</p>
        {(tables.data?.tables ?? []).map((t) => (
          <button
            key={t.name}
            type="button"
            className={cn("flex justify-between gap-2 px-3 py-0.5 text-left hover:bg-surface-2", cols?.table === t.name && "bg-surface-2")}
            onClick={() => void showColumns(t.name)}
            title={`${t.name}: about ${t.estimated_rows} rows. Click to browse it.`}
          >
            <span className="truncate font-mono">{t.name}</span>
            <span className="text-muted">{t.estimated_rows >= 0 ? t.estimated_rows : "?"}</span>
          </button>
        ))}
      </div>
      <div className="flex min-w-0 flex-1 flex-col">
        <div className="flex flex-col gap-1.5 border-b border-border p-2">
          <textarea
            aria-label="SQL"
            spellCheck={false}
            className="h-20 w-full resize-y rounded border border-border bg-surface-2 p-2 font-mono text-xs"
            value={sql}
            onChange={(e) => setSql(e.target.value)}
            onKeyDown={(e) => {
              if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
                e.preventDefault();
                void run();
              }
            }}
          />
          <div className="flex flex-wrap items-center gap-2">
            <button type="button" disabled={busy} className={cn(BTN, "bg-accent text-white hover:bg-accent")} onClick={() => void run()}>
              {busy ? "Running..." : "Run (Ctrl+Enter)"}
            </button>
            <select
              aria-label="Mode"
              className={cn(BTN, mode === "commit" && "border-danger text-danger")}
              value={mode}
              onChange={(e) => setMode(e.target.value as Mode)}
            >
              <option value="read">Read only (safe)</option>
              <option value="preview">Preview changes (rolled back)</option>
              <option value="commit">Run and SAVE changes</option>
            </select>
            <select
              aria-label="Earlier statements"
              className={cn(BTN, "max-w-48")}
              value=""
              onChange={(e) => e.target.value && setSql(e.target.value)}
            >
              <option value="">History</option>
              {past.map((p) => (
                <option key={p} value={p}>
                  {p.slice(0, 80)}
                </option>
              ))}
            </select>
            <button type="button" className={BTN} onClick={() => void copyText(sql)}>
              Copy SQL
            </button>
            {result && (
              <>
                <button type="button" className={BTN} onClick={() => void copyText(tsv(result))}>
                  Copy result (TSV)
                </button>
                <button type="button" className={BTN} onClick={() => void copyText(json(result))}>
                  Copy result (JSON)
                </button>
              </>
            )}
          </div>
          {mode === "commit" && (
            <p className="text-xs text-danger">This changes the real database and you cannot undo it. Take a backup first. Use Preview to see what it does.</p>
          )}
          {cols && (
            <p className="truncate text-xs text-muted" title={cols.list.map((c) => `${c.name} ${c.data_type}`).join(", ")}>
              <span className="font-mono">{cols.table}</span>: {cols.list.map((c) => `${c.name} (${c.data_type}${c.nullable ? "" : ", not null"})`).join(", ")}
            </p>
          )}
        </div>
        <div className="min-h-0 flex-1 overflow-auto text-xs">
          {error && <pre className="whitespace-pre-wrap p-3 font-mono text-danger">{error}</pre>}
          {result && (
            <>
              <p className="sticky left-0 px-3 py-1 text-muted">
                {result.columns.length > 0
                  ? `${result.row_count} row${result.row_count === 1 ? "" : "s"}${result.truncated ? " (first 1000 shown)" : ""}`
                  : `${result.rows_affected} row${result.rows_affected === 1 ? "" : "s"} affected`}
                {" · "}
                {result.elapsed_ms} ms
                {result.mode === "preview" && " · rolled back, nothing was saved"}
                {result.committed && " · SAVED"}
              </p>
              {result.columns.length > 0 && (
                <table className="min-w-full border-collapse font-mono">
                  <thead className="sticky top-0 bg-surface">
                    <tr>
                      {result.columns.map((c, i) => (
                        // biome-ignore lint/suspicious/noArrayIndexKey: columns can repeat a name
                        <th key={i} className="whitespace-nowrap border-b border-border px-2 py-1 text-left font-medium">
                          {c}
                        </th>
                      ))}
                    </tr>
                  </thead>
                  <tbody>
                    {result.rows.map((row, r) => (
                      // biome-ignore lint/suspicious/noArrayIndexKey: rows have no id
                      <tr key={r} className="border-b border-border/40 hover:bg-surface-2">
                        {row.map((cell, i) => (
                          // biome-ignore lint/suspicious/noArrayIndexKey: cells have no id
                          <td key={i} className="max-w-96 truncate px-2 py-0.5" title={cell ?? "NULL"}>
                            {cell === null ? <span className="text-muted">NULL</span> : cell}
                          </td>
                        ))}
                      </tr>
                    ))}
                  </tbody>
                </table>
              )}
            </>
          )}
          {!result && !error && <p className="p-3 text-muted">Run a statement. Read only mode cannot change anything. Click a table on the left to browse it.</p>}
        </div>
      </div>
    </div>
  );
}

