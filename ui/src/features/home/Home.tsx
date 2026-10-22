import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { ArrowRight, CircleCheck, Plus, Upload } from "lucide-react";
import { type ReactNode, useState } from "react";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import { Button, ErrorNote, Page, PageHeader, Pill, Skeleton } from "@/components/ui";
import { AddAccount } from "@/features/accounts/AddAccount";
import { statsQuery, tone } from "@/features/campaigns/hooks";
import { NewCampaign } from "@/features/campaigns/NewCampaign";
import { ProgressBar, progressOf } from "@/features/campaigns/Progress";
import { ImportModal } from "@/features/leads/ImportModal";
import { dailyQuery } from "@/features/reports/DailyChart";
import { presetRange } from "@/features/reports/parts";
import { useUser } from "@/features/settings/useUser";
import { GetStarted } from "./GetStarted";
import { cn } from "@/lib/cn";
import { fmtAgo, fmtInt, fmtPct } from "@/lib/format";
import { say } from "@/lib/words";

/** The live campaigns first, then the paused ones, then the drafts. */
const ORDER: Record<string, number> = { active: 0, paused: 1, draft: 2 };

/** The start page of the Aikido design: the last 7 days, the campaigns, what needs you, the replies and the inboxes. */
export function Home({ ws }: { ws: string }) {
  const { user } = useUser();
  const [open, setOpen] = useState<null | "campaign" | "leads" | "account">(null);
  const close = () => setOpen(null);
  const first = user?.name.trim().split(/\s+/)[0] ?? "";
  const hour = new Date().getHours();
  const greeting = hour < 12 ? "Good morning" : hour < 18 ? "Good afternoon" : "Good evening";
  const today = new Date().toLocaleDateString(undefined, {
    weekday: "long",
    day: "numeric",
    month: "long",
  });
  return (
    <Page>
      <PageHeader
        title={first ? `${greeting}, ${first}` : greeting}
        sub={today}
        actions={
          <>
            <Button variant="secondary" onClick={() => setOpen("leads")}>
              <Upload size={15} aria-hidden="true" /> Import leads
            </Button>
            <Button onClick={() => setOpen("campaign")}>
              <Plus size={15} aria-hidden="true" /> New campaign
            </Button>
          </>
        }
      />
      <GetStarted ws={ws} />
      <Week />
      <div className="grid gap-5 lg:grid-cols-3">
        <Campaigns ws={ws} onNew={() => setOpen("campaign")} className="lg:col-span-2" />
        <Attention />
        <Replies ws={ws} className="lg:col-span-2" />
        <Inboxes ws={ws} onAdd={() => setOpen("account")} />
      </div>
      <NewCampaign open={open === "campaign"} onClose={close} ws={ws} />
      <ImportModal open={open === "leads"} onClose={close} />
      <AddAccount open={open === "account"} onClose={close} />
    </Page>
  );
}

/** A box of the Home page, with a small title bar. */
function Panel({
  title,
  link,
  children,
  className,
}: {
  title: string;
  link?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section
      aria-label={title}
      className={cn(
        "flex min-w-0 flex-col overflow-hidden rounded-(--radius-card) border border-border bg-surface shadow-(--shadow-card)",
        className,
      )}
    >
      <header className="flex h-11 shrink-0 items-center justify-between gap-2 border-b border-border px-4">
        <h2 className="ak-panel-title">{title}</h2>
        {link}
      </header>
      <div className="min-h-0 flex-1">{children}</div>
    </section>
  );
}

function PanelLink({
  to,
  ws,
  children,
}: {
  to: "/w/$ws/campaigns" | "/w/$ws/inbox" | "/w/$ws/accounts";
  ws: string;
  children: ReactNode;
}) {
  return (
    <Link to={to} params={{ ws }} className="flex items-center gap-1 text-xs text-muted hover:text-text">
      {children} <ArrowRight size={13} aria-hidden="true" />
    </Link>
  );
}

/** Four numbers of the last 7 days, each with a small line of its days. */
function Week() {
  const d = useQuery(dailyQuery(presetRange("7")));
  if (d.isError) return <ErrorNote error={d.error} onRetry={() => d.refetch()} />;
  if (d.isPending) return <Skeleton rows={2} className="mb-6" />;
  const t = d.data.totals;
  const days = d.data.days;
  const rate = (a: number, b: number) => (b > 0 ? a / b : 0);
  const tiles = [
    { label: "Sent", value: fmtInt(t.sent), values: days.map((x) => x.sent), color: "var(--info)" },
    {
      label: "Replies",
      value: fmtInt(t.replied),
      values: days.map((x) => x.replied),
      color: "var(--success)",
    },
    {
      label: "Reply rate",
      value: fmtPct(rate(t.replied, t.sent)),
      values: days.map((x) => rate(x.replied, x.sent)),
      color: "var(--success)",
    },
    {
      label: "Bounce rate",
      value: fmtPct(rate(t.bounced, t.sent)),
      values: days.map((x) => rate(x.bounced, x.sent)),
      color: "var(--danger)",
    },
  ];
  return (
    <section
      aria-label="The last 7 days"
      className="mb-5 grid grid-cols-2 overflow-hidden rounded-(--radius-card) border border-border bg-surface shadow-(--shadow-card) lg:grid-cols-4"
    >
      {tiles.map((k, i) => (
        <div
          key={k.label}
          className={cn(
            "flex flex-col gap-2 border-border p-4",
            i % 2 === 1 && "border-l",
            i >= 2 && "border-t lg:border-t-0",
            i === 2 && "lg:border-l",
          )}
        >
          <p className="ui-label flex items-center justify-between text-xs text-muted">
            {k.label}
            <span className="normal-case tracking-normal">7 days</span>
          </p>
          <div className="flex items-end justify-between gap-3">
            <p className="text-2xl leading-8 font-semibold tabular-nums">{k.value}</p>
            <Spark values={k.values} color={k.color} />
          </div>
        </div>
      ))}
    </section>
  );
}

/** A tiny line of the days. It only shows the shape; the number is next to it. */
function Spark({ values, color }: { values: number[]; color: string }) {
  const w = 88;
  const h = 28;
  if (values.length < 2) return null;
  const max = Math.max(...values) || 1;
  const points = values
    .map((v, i) => `${((i / (values.length - 1)) * w).toFixed(1)},${(h - 2 - (v / max) * (h - 4)).toFixed(1)}`)
    .join(" ");
  return (
    <svg viewBox={`0 0 ${w} ${h}`} width={w} height={h} aria-hidden="true" className="shrink-0">
      <polyline
        points={points}
        fill="none"
        stroke={color}
        strokeWidth="1.75"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

function Campaigns({ ws, onNew, className }: { ws: string; onNew: () => void; className?: string }) {
  const list = useQuery(q.campaigns);
  const rows = (list.data?.campaigns ?? [])
    .filter((c) => c.status in ORDER)
    .sort((a, b) => (ORDER[a.status] ?? 9) - (ORDER[b.status] ?? 9))
    .slice(0, 6);
  return (
    <Panel
      title="Campaigns"
      className={className}
      link={
        <PanelLink to="/w/$ws/campaigns" ws={ws}>
          All campaigns
        </PanelLink>
      }
    >
      {list.isPending && <Skeleton rows={3} className="p-4" />}
      {list.isError && (
        <div className="p-4">
          <ErrorNote error={list.error} onRetry={() => list.refetch()} />
        </div>
      )}
      {list.data && rows.length === 0 && (
        <div className="flex flex-col items-start gap-3 p-4">
          <p className="text-muted">No campaigns are running. A campaign sends a set of emails to your leads.</p>
          <Button variant="secondary" onClick={onNew}>
            <Plus size={15} aria-hidden="true" /> New campaign
          </Button>
        </div>
      )}
      {rows.length > 0 && (
        <ul>
          {rows.map((c) => (
            <CampaignLine key={c.id} ws={ws} c={c} />
          ))}
        </ul>
      )}
    </Panel>
  );
}

function CampaignLine({ ws, c }: { ws: string; c: S["CampaignSummary"] }) {
  const stats = useQuery({ ...statsQuery(c.id), enabled: c.status !== "draft" });
  const progress = c.status === "draft" ? null : progressOf(c.counts);
  const s = stats.data;
  return (
    <li className="border-t border-border first:border-t-0">
      <Link
        to="/w/$ws/campaigns/$id"
        params={{ ws, id: c.id }}
        className="grid grid-cols-[minmax(0,1fr)_auto] items-center gap-x-4 gap-y-1 px-4 py-3 hover:bg-(--row-hover) sm:grid-cols-[minmax(0,1fr)_7rem_9rem_auto]"
      >
        <span className="min-w-0">
          <span className="block truncate font-medium">{c.name}</span>
          <span className="block text-xs text-muted">
            {fmtInt(c.counts.enrolled + c.counts.pending)} leads
          </span>
        </span>
        <span className="hidden sm:block">
          {progress == null ? <span className="text-muted">-</span> : <ProgressBar value={progress} />}
        </span>
        <span className="hidden text-xs text-muted tabular-nums sm:block">
          {s ? (
            <>
              <b className="font-semibold text-text">{fmtInt(s.sent)}</b> sent ·{" "}
              <b className="font-semibold text-text">{fmtInt(s.replied)}</b> replies
            </>
          ) : (
            "No emails yet"
          )}
        </span>
        <span className="justify-self-end">
          <Pill dot tone={tone(c.status)}>
            {say("campaign", c.status)}
          </Pill>
        </span>
      </Link>
    </li>
  );
}

/** The open alerts: what needs a person now. */
function Attention() {
  const alerts = useQuery(q.alerts);
  const list = alerts.data ?? [];
  return (
    <Panel title="Needs you">
      {alerts.isPending && <Skeleton rows={2} className="p-4" />}
      {alerts.isError && (
        <div className="p-4">
          <ErrorNote error={alerts.error} onRetry={() => alerts.refetch()} />
        </div>
      )}
      {alerts.data && list.length === 0 && (
        <div className="flex items-center gap-3 p-4 text-muted">
          <CircleCheck size={18} aria-hidden="true" className="shrink-0 text-success" />
          All clear. Nothing needs you now.
        </div>
      )}
      {list.length > 0 && (
        <ul>
          {list.slice(0, 6).map((a) => (
            <li key={a.id} className="border-t border-border first:border-t-0">
              <Link
                to={a.link as never}
                className="flex gap-3 px-4 py-3 hover:bg-(--row-hover)"
              >
                <span
                  aria-hidden="true"
                  className={cn(
                    "mt-1.5 size-2 shrink-0 rounded-full",
                    a.severity === "error"
                      ? "bg-danger"
                      : a.severity === "warning"
                        ? "bg-warning"
                        : "bg-info",
                  )}
                />
                <span className="min-w-0">
                  <span className="block font-medium">{a.title}</span>
                  <span className="block text-xs text-muted">{a.detail}</span>
                  <span className="block text-xs text-muted">{fmtAgo(a.since)}</span>
                </span>
              </Link>
            </li>
          ))}
        </ul>
      )}
    </Panel>
  );
}

/** The latest replies of leads. */
function Replies({ ws, className }: { ws: string; className?: string }) {
  const replies = useQuery({
    queryKey: ["inbox", "latest"],
    queryFn: () =>
      http.get<S["ListMessagesReply"]>("/v1/replies/messages", {
        query: { label: "reply", limit: 6 },
      }),
    refetchInterval: 60_000,
  });
  const list = replies.data?.messages ?? [];
  return (
    <Panel
      title="Latest replies"
      className={className}
      link={
        <PanelLink to="/w/$ws/inbox" ws={ws}>
          Inbox
        </PanelLink>
      }
    >
      {replies.isPending && <Skeleton rows={3} className="p-4" />}
      {replies.isError && (
        <div className="p-4">
          <ErrorNote error={replies.error} onRetry={() => replies.refetch()} />
        </div>
      )}
      {replies.data && list.length === 0 && (
        <p className="p-4 text-muted">No replies yet. They show here when leads answer.</p>
      )}
      {list.length > 0 && (
        <ul>
          {list.map((m) => {
            const who = m.from_name?.trim() || m.from_email;
            return (
              <li key={m.id} className="border-t border-border first:border-t-0">
                <Link
                  to="/w/$ws/inbox"
                  params={{ ws }}
                  search={{ m: m.id }}
                  className="flex items-start gap-3 px-4 py-3 hover:bg-(--row-hover)"
                >
                  <span
                    aria-hidden="true"
                    className="flex size-8 shrink-0 items-center justify-center rounded-full bg-surface-2 text-xs font-semibold uppercase"
                  >
                    {who.slice(0, 1)}
                  </span>
                  <span className="min-w-0 flex-1">
                    <span className="flex items-baseline justify-between gap-3">
                      <span className="truncate font-medium">{who}</span>
                      <span className="shrink-0 text-xs text-muted">{fmtAgo(m.received_at)}</span>
                    </span>
                    <span className="block truncate text-sm">{m.subject || "(no subject)"}</span>
                    {m.reply_text && (
                      <span className="block truncate text-xs text-muted">{m.reply_text}</span>
                    )}
                  </span>
                </Link>
              </li>
            );
          })}
        </ul>
      )}
    </Panel>
  );
}

const ACCOUNT_DOT: Record<string, string> = {
  active: "bg-success",
  paused: "bg-warning",
  error: "bg-danger",
  disconnected: "bg-danger",
};

/** The email accounts and whether each one can send. */
function Inboxes({ ws, onAdd }: { ws: string; onAdd: () => void }) {
  const accounts = useQuery(q.accounts);
  const list = accounts.data ?? [];
  return (
    <Panel
      title="Inboxes"
      link={
        <PanelLink to="/w/$ws/accounts" ws={ws}>
          All accounts
        </PanelLink>
      }
    >
      {accounts.isPending && <Skeleton rows={3} className="p-4" />}
      {accounts.isError && (
        <div className="p-4">
          <ErrorNote error={accounts.error} onRetry={() => accounts.refetch()} />
        </div>
      )}
      {accounts.data && list.length === 0 && (
        <div className="flex flex-col items-start gap-3 p-4">
          <p className="text-muted">Connect an inbox to send from it.</p>
          <Button variant="secondary" onClick={onAdd}>
            <Plus size={15} aria-hidden="true" /> Add email account
          </Button>
        </div>
      )}
      {list.length > 0 && (
        <ul>
          {list.slice(0, 8).map((a) => (
            <li key={a.id} className="border-t border-border first:border-t-0">
              <Link
                to="/w/$ws/accounts/$id"
                params={{ ws, id: a.id }}
                className="flex items-center gap-3 px-4 py-2.5 hover:bg-(--row-hover)"
              >
                <span
                  aria-hidden="true"
                  className={cn("size-2 shrink-0 rounded-full", ACCOUNT_DOT[a.status] ?? "bg-border")}
                />
                <span className="min-w-0 flex-1 truncate">{a.email}</span>
                <span className="shrink-0 text-xs text-muted">{say("account", a.status)}</span>
              </Link>
            </li>
          ))}
          {list.length > 8 && (
            <li className="border-t border-border px-4 py-2.5 text-xs text-muted">
              And {list.length - 8} more.
            </li>
          )}
        </ul>
      )}
    </Panel>
  );
}
