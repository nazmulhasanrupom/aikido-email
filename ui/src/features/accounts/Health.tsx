import { useQuery } from "@tanstack/react-query";
import { ShieldAlert, ShieldCheck } from "lucide-react";
import { type ReactNode, useState } from "react";
import { http, type S } from "@/api/http";
import { Button, CopyButton, ErrorNote, Notice, Pill, Skeleton } from "@/components/ui";
import { fmtDateTime } from "@/lib/format";

export const healthQuery = (id: string, selector = "") => ({
  queryKey: ["accounts", id, "health", selector],
  queryFn: () =>
    http.get<S["HealthReport"]>("/v1/accounts/{id}/health", {
      path: { id },
      query: selector ? { selector } : {},
    }),
  staleTime: 60_000,
});

const TONE = { ok: "success", warn: "warning", fail: "danger", unknown: "neutral" } as const;
const WORD = {
  ok: "Verified",
  warn: "Look at this",
  fail: "Missing",
  unknown: "Not known",
} as const;

const ABOUT: Record<string, string> = {
  mx: "Mail Exchange: where mail and replies for the domain go.",
  spf: "Sender Policy Framework: the servers that may send for the domain.",
  dkim: "DomainKeys Identified Mail: a signature on every email.",
  dmarc: "Domain-based Message Authentication: what to do when SPF or DKIM fails.",
  a: "Address record: the domain points at a real website.",
};

const BONUS: Record<string, string> = {
  perfect_setup: "Perfect technical setup",
  maturity: "Account maturity",
  perfect_performance: "Perfect performance",
};

/** The color of a score: green from 80, orange from 60, red below. */
function scoreColor(n: number) {
  return n >= 80 ? "var(--success)" : n >= 60 ? "var(--warning)" : "var(--danger)";
}
function scoreTone(n: number) {
  return n >= 80 ? "success" : n >= 60 ? "warning" : "danger";
}

/** The score in a small pill. Used in the account list. */
export function HealthChip({ accountId }: { accountId: string }) {
  const r = useQuery(healthQuery(accountId));
  if (r.isPending) return <span className="text-muted">...</span>;
  if (!r.data) return null;
  return (
    <span title="Health score: DNS records, sending power and blacklists">
      <Pill dot tone={scoreTone(r.data.score)}>
        {r.data.score} / 100
      </Pill>
    </span>
  );
}

function Ring({ score }: { score: number }) {
  const R = 34;
  const C = 2 * Math.PI * R;
  return (
    <div className="relative size-24 shrink-0">
      <svg viewBox="0 0 80 80" className="size-24 -rotate-90" aria-hidden="true">
        <circle cx="40" cy="40" r={R} fill="none" stroke="var(--surface-2)" strokeWidth="7" />
        <circle
          cx="40"
          cy="40"
          r={R}
          fill="none"
          stroke={scoreColor(score)}
          strokeWidth="7"
          strokeLinecap="round"
          strokeDasharray={`${(score / 100) * C} ${C}`}
        />
      </svg>
      <span className="absolute inset-0 flex items-center justify-center text-2xl font-semibold tabular-nums">
        {score}
      </span>
    </div>
  );
}

function Bar({ value }: { value: number }) {
  return (
    <div className="h-1.5 w-full overflow-hidden rounded-full bg-surface-2">
      <div
        className="h-full rounded-full"
        style={{ width: `${value}%`, background: scoreColor(value) }}
      />
    </div>
  );
}

function MetricRow({ label, m }: { label: string; m?: S["Metric"] | null }) {
  if (!m) return null;
  return (
    <div className="flex flex-col gap-1.5">
      <div className="flex justify-between text-sm">
        <span className="font-medium">{label}</span>
        <span className="tabular-nums">{m.score} / 100</span>
      </div>
      <Bar value={m.score} />
      <p className="text-xs text-muted">{m.detail}</p>
    </div>
  );
}

function Tile({ label, value, sub }: { label: string; value: string; sub?: string }) {
  return (
    <div className="flex-1 rounded-(--radius-control) border border-border p-3">
      <p className="text-xs text-muted">{label}</p>
      <p className="mt-1 font-semibold">{value}</p>
      {sub && <p className="text-xs text-muted">{sub}</p>}
    </div>
  );
}

function Section({ title, sub, children }: { title: string; sub: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-3 border-t border-border pt-4">
      <div>
        <h3 className="font-semibold">{title}</h3>
        <p className="text-sm text-muted">{sub}</p>
      </div>
      {children}
    </section>
  );
}

function DnsRow({ item }: { item: S["DnsItem"] }) {
  const s = item.check.status as keyof typeof TONE;
  return (
    <li className="flex flex-col gap-1.5 rounded-(--radius-control) border border-border p-3">
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-medium">{item.label}</span>
        <span className="rounded bg-surface-2 px-1.5 text-xs font-medium tabular-nums">
          +{item.earned}
          {item.earned < item.points && <span className="text-muted"> of {item.points}</span>}
        </span>
        <span className="ml-auto">
          <Pill dot tone={TONE[s] ?? "neutral"}>
            {WORD[s] ?? item.check.status}
          </Pill>
        </span>
      </div>
      <p className="text-xs text-muted">{ABOUT[item.key]}</p>
      {s !== "ok" && <p className="text-sm">{item.check.summary}</p>}
      {item.check.found && (
        <code className="block truncate text-xs text-muted">{item.check.found}</code>
      )}
      {item.check.fix && (
        <div className="flex items-start gap-2 rounded bg-surface-2 p-2 text-xs">
          <code className="flex-1 whitespace-pre-wrap break-all">{item.check.fix}</code>
          <CopyButton text={item.check.fix} />
        </div>
      )}
    </li>
  );
}

/** One score for the account: DNS records, sending power and blacklists. */
export function HealthCard({ accountId }: { accountId: string }) {
  const [typed, setTyped] = useState("");
  const [selector, setSelector] = useState("");
  const r = useQuery(healthQuery(accountId, selector));
  const h = r.data;
  return (
    <div className="flex flex-col gap-4" aria-label="Health check">
      <div className="flex items-center justify-between gap-2">
        <h2 className="font-semibold">Health{h ? `: ${h.domain}` : ""}</h2>
        <Button
          variant="secondary"
          className="h-8"
          onClick={() => void r.refetch()}
          busy={r.isFetching}
        >
          Check again
        </Button>
      </div>
      {r.isPending && <Skeleton rows={4} />}
      {r.isError && <ErrorNote error={r.error} onRetry={() => void r.refetch()} />}
      {h && (
        <>
          <div className="flex flex-wrap items-center gap-4">
            <div className="flex items-center gap-3">
              <Ring score={h.score} />
              <div>
                <p className="text-sm text-muted">Health score</p>
                <Pill dot tone={h.ready ? "success" : "danger"}>
                  {h.ready ? "Ready to send" : "Not ready to send"}
                </Pill>
              </div>
            </div>
            <div className="flex min-w-64 flex-1 flex-wrap gap-3">
              <Tile
                label="DNS health"
                value={`${h.dns.score} / 100`}
                sub={`Ready from ${h.ready_min}`}
              />
              <Tile
                label="Sending power"
                value={h.sending.score == null ? "Not enough sends" : `${h.sending.score} / 100`}
                sub={`${h.sending.sent} sent in 30 days`}
              />
              <Tile
                label="Blacklists"
                value={
                  h.blacklists.listed.length
                    ? `Listed on ${h.blacklists.listed.length}`
                    : "Not listed"
                }
                sub={`${h.blacklists.checked} lookups`}
              />
            </div>
          </div>

          <Section
            title="DNS records"
            sub="Email authentication and delivery. Each record adds points."
          >
            <ul className="grid gap-2 md:grid-cols-2">
              {h.dns.items.map((i) => (
                <DnsRow key={i.key} item={i} />
              ))}
            </ul>
            <form
              className="flex flex-wrap items-end gap-2 text-sm"
              onSubmit={(e) => {
                e.preventDefault();
                setSelector(typed.trim());
              }}
            >
              <label className="flex flex-col gap-1">
                <span className="text-muted">Your DKIM selector (if it is not a common one)</span>
                <input
                  value={typed}
                  onChange={(e) => setTyped(e.target.value)}
                  placeholder="for example s1"
                  className="h-8 w-48 rounded border border-border bg-surface-2 px-2"
                />
              </label>
              <Button type="submit" variant="secondary" className="h-8">
                Check
              </Button>
            </form>
          </Section>

          <Section
            title="Sending power"
            sub="From the emails this account sent in the last 30 days."
          >
            {h.sending.score == null ? (
              <p className="text-sm text-muted">
                Sending power starts after 10 sent emails. Until then the score is the DNS health
                only.
              </p>
            ) : (
              <div className="grid gap-4 md:grid-cols-2">
                <MetricRow label="Sending performance" m={h.sending.performance} />
                <MetricRow label="Bounce management" m={h.sending.bounces} />
              </div>
            )}
            {h.sending.bonuses.length > 0 && (
              <div className="flex flex-wrap items-center gap-2 text-sm">
                <span className="text-muted">Bonuses</span>
                {h.sending.bonuses.map((b) => (
                  <Pill key={b} tone="accent">
                    {BONUS[b] ?? b}
                  </Pill>
                ))}
              </div>
            )}
          </Section>

          <Section
            title="Blacklist status"
            sub={`${h.blacklists.targets.join(", ")} looked up on public DNS blacklists.`}
          >
            {h.blacklists.listed.length === 0 ? (
              <div className="flex items-center gap-3 rounded-(--radius-control) border border-success/40 bg-success/10 p-3">
                <ShieldCheck size={20} className="text-success" aria-hidden="true" />
                <div>
                  <p className="font-medium">No blacklists found</p>
                  <p className="text-sm text-muted">
                    The domain and IP are not on the lists we looked at.
                  </p>
                </div>
              </div>
            ) : (
              <ul className="flex flex-col gap-2">
                {h.blacklists.listed.map((l) => (
                  <li
                    key={l.list + l.target}
                    className="flex items-center gap-3 rounded-(--radius-control) border border-danger/40 bg-danger/10 p-3"
                  >
                    <ShieldAlert size={18} className="text-danger" aria-hidden="true" />
                    <span className="font-medium">{l.target}</span>
                    <span className="text-sm text-muted">
                      is on {l.list} (code {l.answer})
                    </span>
                  </li>
                ))}
              </ul>
            )}
            {h.blacklists.unknown > 0 && (
              <p className="text-xs text-muted">
                {h.blacklists.unknown} of {h.blacklists.checked} lists did not answer. Some lists
                refuse queries from big public DNS servers.
              </p>
            )}
            {h.blacklists.note && <Notice tone="info">{h.blacklists.note}</Notice>}
          </Section>

          <p className="text-xs text-muted">
            Last checked: {fmtDateTime(h.checked_at)}. Answers are kept for one hour.
          </p>
        </>
      )}
    </div>
  );
}
