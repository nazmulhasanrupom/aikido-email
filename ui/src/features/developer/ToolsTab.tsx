import { Link } from "@tanstack/react-router";
import { useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http } from "@/api/http";
import { copyText, fmtBytes, fmtUptime, useInfo } from "./api";

const BTN = "h-7 rounded border border-border bg-surface-2 px-2 text-xs hover:bg-accent-soft";

const ACTIONS: [string, string, string][] = [
  ["followups_tick", "Run follow-ups now", "Make the jobs of steps that are due."],
  ["feeder_tick", "Run the campaign feeder now", "Enroll waiting leads (respects the daily limit)."],
  ["sending_tick", "Run sending now", "Send what the rules allow, without waiting for the timer."],
  ["tracking_tick", "Check tracking domains now", "Look at the CNAME of every tracking domain."],
  ["reload_certs", "Reload tracking certificates", "Read the saved certificates again."],
  ["test_log", "Write test log lines", "One line of each level, to see the log works."],
];

const COUNT_LABELS: Record<string, string> = {
  jobs_queued: "Emails queued",
  jobs_sending: "Sending now",
  jobs_stuck: "Stuck (unknown result)",
  jobs_failed_24h: "Failed, last 24 h",
  jobs_sent_24h: "Sent, last 24 h",
  enrollments_open: "Open sequences",
  enrollments_failed: "Failed sequences",
  accounts: "Email accounts",
  accounts_not_active: "Accounts not active",
  leads: "Leads",
  campaigns_active: "Active campaigns",
  opens_24h: "Opens, last 24 h",
  tracking_domains_not_green: "Tracking domains not green",
};
const BAD = new Set(["jobs_stuck", "jobs_failed_24h", "enrollments_failed", "accounts_not_active", "tracking_domains_not_green"]);

/** Facts about the server, and buttons that run a part of it now. */
export function ToolsTab({ ws, active }: { ws: string; active: boolean }) {
  const info = useInfo(active);
  const [busy, setBusy] = useState<string | null>(null);
  const [last, setLast] = useState<string | null>(null);

  async function run(name: string) {
    setBusy(name);
    try {
      const r = await http.post<{ ok: boolean; message: string }>(`/v1/dev/actions/${name}`);
      setLast(`${name}: ${r.message}`);
      toast.success("Done. See the log.");
      void info.refetch();
    } catch (e) {
      setLast(`${name}: ${messageOf(e)}`);
      toast.error(messageOf(e));
    } finally {
      setBusy(null);
    }
  }
  const d = info.data;
  return (
    <div className="h-full overflow-auto p-3 text-xs">
      <div className="grid gap-4 lg:grid-cols-2">
        <section className="flex flex-col gap-2">
          <h3 className="font-medium">Controls</h3>
          <div className="grid gap-1.5 sm:grid-cols-2">
            {ACTIONS.map(([name, label, help]) => (
              <button
                key={name}
                type="button"
                disabled={busy !== null}
                className="rounded border border-border bg-surface-2 p-2 text-left hover:bg-accent-soft disabled:opacity-60"
                onClick={() => void run(name)}
                title={help}
              >
                <span className="font-medium">{busy === name ? "Running..." : label}</span>
                <span className="block text-muted">{help}</span>
              </button>
            ))}
          </div>
          {last && <p className="rounded bg-surface-2 p-2 font-mono">{last}</p>}
          <p className="text-muted">
            To pause or resume the engines, open the{" "}
            <Link to="/w/$ws/status" params={{ ws }} className="text-accent underline">
              Status page
            </Link>
            . To change what is sent now, use "Send now" on a campaign lead.
          </p>
        </section>
        <section className="flex flex-col gap-2">
          <div className="flex items-center gap-2">
            <h3 className="font-medium">Server</h3>
            {d && (
              <button type="button" className={BTN} onClick={() => void copyText(JSON.stringify(d, null, 2))}>
                Copy all as JSON
              </button>
            )}
          </div>
          {!d && <p className="text-muted">Loading...</p>}
          {d && (
            <>
              <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5">
                <dt className="text-muted">Version</dt>
                <dd>{d.version}</dd>
                <dt className="text-muted">Up for</dt>
                <dd>{fmtUptime(d.uptime_secs)}</dd>
                <dt className="text-muted">Database</dt>
                <dd>
                  PostgreSQL {d.database_version}, {fmtBytes(d.database_bytes)}
                </dd>
                <dt className="text-muted">Connections</dt>
                <dd>
                  {d.pool_size} open, {d.pool_idle} idle
                </dd>
                <dt className="text-muted">Log</dt>
                <dd>{d.log_buffer} lines kept</dd>
              </dl>
              <div className="grid grid-cols-2 gap-1.5 sm:grid-cols-3">
                {Object.entries(d.counts).map(([k, v]) => (
                  <div key={k} className="rounded border border-border p-1.5">
                    <div className={BAD.has(k) && (v ?? 0) > 0 ? "text-base font-semibold text-danger" : "text-base font-semibold"}>{v ?? "?"}</div>
                    <div className="text-muted">{COUNT_LABELS[k] ?? k}</div>
                  </div>
                ))}
              </div>
              <details>
                <summary className="cursor-pointer text-muted">Settings of the server (no secrets)</summary>
                <pre className="mt-1 overflow-auto rounded bg-surface-2 p-2">{JSON.stringify(d.config, null, 2)}</pre>
              </details>
            </>
          )}
        </section>
      </div>
    </div>
  );
}
