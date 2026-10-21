import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, useNavigate } from "@tanstack/react-router";
import { Inbox as InboxIcon } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { toast } from "sonner";
import { authStateQuery } from "@/api/auth";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import {
  Button,
  ConfirmDialog,
  EmptyState,
  ErrorNote,
  Page,
  PageHeader,
  Pill,
  Select,
  Skeleton,
  TextArea,
} from "@/components/ui";
import { cn } from "@/lib/cn";
import { readStored, writeStored } from "@/lib/safe-storage";
import { fmtAgo, fmtDateTime } from "@/lib/format";
import { INTERESTS, LABELS, say } from "@/lib/words";

type Search = {
  label?: string;
  campaign_id?: string;
  account_id?: string;
  interest?: string;
  m?: string;
  /** `no`: only replies whose lead has no interest set (not handled). */
  handled?: string;
};
const str = (v: unknown) => (typeof v === "string" && v ? v : undefined);

export const Route = createFileRoute("/w/$ws/inbox")({
  validateSearch: (s: Record<string, unknown>): Search =>
    Object.fromEntries(
      ["label", "campaign_id", "account_id", "interest", "m", "handled"]
        .map((k) => [k, str(s[k])])
        .filter(([, v]) => v),
    ),
  component: Inbox,
});

const DONE_KEY = "aikido.inbox.done";
function readDone(): string[] {
  try {
    const v = JSON.parse(readStored(DONE_KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x) => typeof x === "string").slice(-2000) : [];
  } catch {
    return [];
  }
}
function writeDone(ids: string[]) {
  writeStored(DONE_KEY, JSON.stringify(ids.slice(-2000)));
}

const CHIPS: [string, string][] = [
  ["reply", "Replies"],
  ["auto_reply", "Out of office"],
  ["unsubscribe", "Unsubscribes"],
  ["bounce", "Bounces"],
  ["complaint", "Spam complaints"],
  ["all", "All"],
];

function Inbox() {
  const { ws } = Route.useParams();
  const search = Route.useSearch();
  const navigate = useNavigate();
  const qc = useQueryClient();
  const auth = useQuery(authStateQuery);
  const accounts = useQuery(q.accounts);
  const campaigns = useQuery(q.campaigns);
  const seen = useRef<string | null>(
    (auth.data?.user?.preferences as { last_seen_inbox_at?: string } | undefined)
      ?.last_seen_inbox_at ?? null,
  );
  const label = search.label ?? "reply";

  // Opening the inbox marks everything as seen (the old value is kept to show what is new).
  useEffect(() => {
    const now = new Date().toISOString();
    http
      .patch("/v1/users/me", { body: { preferences: { last_seen_inbox_at: now } } })
      .then(() => qc.invalidateQueries({ queryKey: ["inbox", "badge"] }))
      .catch(() => {});
  }, [qc]);

  const list = useInfiniteQuery({
    queryKey: ["inbox", label, search.campaign_id, search.account_id, search.handled],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) => {
      const labels =
        label === "bounce"
          ? ["bounce_hard", "bounce_soft", "bounce_block"]
          : label === "all"
            ? [undefined]
            : [label];
      return Promise.all(
        labels.map((l) =>
          http.get<S["ListMessagesReply"]>("/v1/replies/messages", {
            query: {
              label: l,
              campaign_id: search.campaign_id,
              account_id: search.account_id,
              ...(search.handled === "no" ? { handled: false } : {}),
              limit: 50,
              cursor: pageParam,
            },
          }),
        ),
      ).then((rs) => ({
        messages: rs
          .flatMap((r) => r.messages)
          .sort((a, b) => b.received_at.localeCompare(a.received_at)),
        next_cursor: rs.length === 1 ? rs[0]?.next_cursor : undefined,
      }));
    },
    getNextPageParam: (l) => l.next_cursor ?? undefined,
    refetchInterval: 60_000,
  });
  const [done, setDone] = useState<string[]>(() => readDone());
  const all = list.data?.pages.flatMap((p) => p.messages) ?? [];
  // "Not handled" also hides what you marked done with E
  const msgs = search.handled === "no" ? all.filter((m) => !done.includes(m.id)) : all;
  const current = msgs.find((m) => m.id === search.m);
  const go = (p: Partial<Search>) =>
    navigate({ to: "/w/$ws/inbox", params: { ws }, search: { ...search, ...p }, replace: true });
  const setInterest = useMutation({
    mutationFn: (v: string) =>
      http.patch("/v1/leads/{id}", { path: { id: current?.lead_id as string }, body: { interest: v } }),
    onSuccess: () => {
      toast.success("Saved.");
      void qc.invalidateQueries({ queryKey: ["leads"] });
      void qc.invalidateQueries({ queryKey: ["inbox"] });
    },
    onError: (e) => toast.error(messageOf(e)),
  });

  // Fast keys: J / K next and previous, I interested, N not interested, R reply, E done.
  const keys = useRef<(k: string) => void>(() => {});
  keys.current = (k) => {
    const i = msgs.findIndex((m) => m.id === search.m);
    const step = (d: number) => {
      const n = msgs[Math.max(0, Math.min(msgs.length - 1, i + d))];
      if (n) go({ m: n.id });
    };
    if (k === "j") step(1);
    else if (k === "k") step(-1);
    else if (k === "i" && current?.lead_id) setInterest.mutate("interested");
    else if (k === "n" && current?.lead_id) setInterest.mutate("not_interested");
    else if (k === "r") document.querySelector<HTMLTextAreaElement>('section[aria-label="Message"] textarea')?.focus();
    else if (k === "e" && current) {
      const next = [...done, current.id];
      setDone(next);
      writeDone(next);
      step(1);
    }
  };
  useEffect(() => {
    const h = (e: KeyboardEvent) => {
      const t = e.target as HTMLElement | null;
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      if (t && (t.tagName === "INPUT" || t.tagName === "TEXTAREA" || t.tagName === "SELECT" || t.isContentEditable)) return;
      const k = e.key.toLowerCase();
      if ("jkinre".includes(k) && k.length === 1) {
        e.preventDefault();
        keys.current(k);
      }
    };
    window.addEventListener("keydown", h);
    return () => window.removeEventListener("keydown", h);
  }, []);

  return (
    <Page wide>
      <PageHeader title="Inbox" sub="The replies we found in your connected inboxes." />
      <div className="mb-4 flex flex-wrap items-end gap-2">
        <div role="group" aria-label="Kind of email" className="flex flex-wrap gap-1.5">
          {CHIPS.map(([k, l]) => (
            <button
              key={k}
              type="button"
              aria-pressed={label === k}
              onClick={() => go({ label: k === "reply" ? undefined : k, m: undefined })}
              className={cn(
                "h-9 rounded-full border px-3 text-sm",
                label === k ? "border-accent bg-accent-soft font-medium" : "border-border",
              )}
            >
              {l}
            </button>
          ))}
        </div>
        <button
          type="button"
          aria-pressed={search.handled === "no"}
          onClick={() => go({ handled: search.handled === "no" ? undefined : "no", m: undefined })}
          className={cn(
            "h-9 rounded-full border px-3 text-sm",
            search.handled === "no" ? "border-accent bg-accent-soft font-medium" : "border-border",
          )}
        >
          Not handled
        </button>
        <div className="w-44">
          <Select
            label="Campaign"
            value={search.campaign_id ?? ""}
            onChange={(v) => go({ campaign_id: v || undefined })}
            options={[
              { value: "", label: "All campaigns" },
              ...(campaigns.data?.campaigns ?? []).map((c) => ({ value: c.id, label: c.name })),
            ]}
          />
        </div>
        <div className="w-44">
          <Select
            label="Account"
            value={search.account_id ?? ""}
            onChange={(v) => go({ account_id: v || undefined })}
            options={[
              { value: "", label: "All accounts" },
              ...(accounts.data ?? []).map((a) => ({ value: a.id, label: a.email })),
            ]}
          />
        </div>
      </div>
      {list.isPending && <Skeleton />}
      {list.isError && <ErrorNote error={list.error} onRetry={() => list.refetch()} />}
      {list.data && msgs.length === 0 && (
        <EmptyState
          icon={InboxIcon}
          text="Nothing here yet. Replies show up a few minutes after they arrive."
        />
      )}
      {msgs.length > 0 && (
        <p className="mb-2 text-xs text-muted">
          Keys: J next, K previous, I interested, N not interested, R reply, E done.
        </p>
      )}
      {msgs.length > 0 && (
        <div className="grid min-h-[60vh] gap-4 lg:grid-cols-[360px_1fr]">
          <ul
            className="flex max-h-[75vh] flex-col overflow-auto rounded-(--radius-card) border border-border"
            aria-label="Messages"
          >
            {msgs.map((m) => {
              const unread = !!seen.current && m.received_at > seen.current;
              return (
                <li key={m.id}>
                  <button
                    type="button"
                    onClick={() => go({ m: m.id })}
                    aria-current={m.id === search.m}
                    className={cn(
                      "flex w-full flex-col gap-0.5 border-b border-border px-3 py-2.5 text-left hover:bg-surface-2",
                      m.id === search.m && "bg-accent-soft",
                    )}
                  >
                    <span className="flex items-center justify-between gap-2">
                      <span className={cn("truncate", unread && "font-semibold")}>
                        {unread && (
                          <span
                            className="mr-1.5 inline-block size-2 rounded-full bg-accent"
                            aria-label="New"
                          />
                        )}
                        {m.from_name || m.from_email}
                      </span>
                      <span className="shrink-0 text-xs text-muted">{fmtAgo(m.received_at)}</span>
                    </span>
                    <span className="truncate text-xs text-muted">{m.subject}</span>
                    <span className="line-clamp-1 text-xs text-muted">
                      {m.reply_text?.slice(0, 120)}
                    </span>
                    {label === "all" && <Pill>{say("label", m.label)}</Pill>}
                  </button>
                </li>
              );
            })}
            {list.hasNextPage && (
              <li className="p-2">
                <Button variant="secondary" className="w-full" onClick={() => list.fetchNextPage()}>
                  Load more
                </Button>
              </li>
            )}
          </ul>
          <section aria-label="Message">
            {current ? (
              <Message m={current} ws={ws} accounts={accounts.data ?? []} />
            ) : (
              <p className="p-6 text-muted">Choose a message.</p>
            )}
          </section>
        </div>
      )}
    </Page>
  );
}

function Message({
  m,
  ws,
  accounts,
}: {
  m: S["InboundMessage"];
  ws: string;
  accounts: S["AccountView"][];
}) {
  const qc = useQueryClient();
  const lead = useQuery({
    queryKey: ["leads", "one", m.lead_id],
    enabled: !!m.lead_id,
    queryFn: () =>
      http.get<S["LeadDetail"]>("/v1/leads/{id}", { path: { id: m.lead_id as string } }),
  });
  const job = useQuery({
    queryKey: ["job", m.matched_job_id],
    enabled: !!m.matched_job_id,
    queryFn: () =>
      http.get<S["JobDetail"]>("/v1/sending/jobs/{id}", {
        path: { id: m.matched_job_id as string },
      }),
  });
  const [reply, setReply] = useState("");
  const [relabel, setRelabel] = useState<string | null>(null);
  useEffect(() => setReply(""), [m.id]);
  const account = accounts.find((a) => a.id === m.account_id);
  const interest = useMutation({
    mutationFn: (v: string) =>
      http.patch("/v1/leads/{id}", { path: { id: m.lead_id as string }, body: { interest: v } }),
    onSuccess: () => {
      toast.success("Saved.");
      void qc.invalidateQueries({ queryKey: ["leads"] });
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const send = useMutation({
    mutationFn: () => {
      const subject = (m.subject ?? "").match(/^re:/i)
        ? (m.subject as string)
        : `Re: ${m.subject ?? ""}`;
      const refs = [...m.references_ids, m.message_id_header]
        .filter(Boolean)
        .map((r) => (String(r).startsWith("<") ? r : `<${r}>`))
        .join(" ");
      return http.post("/v1/sending/jobs", {
        body: {
          jobs: [
            {
              account_id: m.account_id,
              to_email: m.from_email,
              subject,
              body_text: reply,
              kind: "manual",
              priority: 100,
              in_reply_to: m.message_id_header ?? null,
              references: refs || null,
              provider_thread_id: m.provider_thread_id ?? null,
              lead_id: m.lead_id ?? null,
              campaign_id: m.campaign_id ?? null,
            },
          ],
        },
      });
    },
    onSuccess: () => {
      toast.success("Queued. It goes out in the same thread.");
      setReply("");
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const l = lead.data;
  return (
    <article className="flex flex-col gap-4 rounded-(--radius-card) border border-border p-5">
      <header className="flex flex-wrap items-start justify-between gap-2">
        <div>
          <h2 className="font-semibold">{m.from_name || m.from_email}</h2>
          <p className="text-sm text-muted">
            {m.from_email} &middot; {fmtDateTime(m.received_at)}
          </p>
          <p className="text-sm">{m.subject}</p>
        </div>
        <div className="flex items-center gap-2">
          <Pill tone={m.label === "reply" ? "success" : "neutral"}>{say("label", m.label)}</Pill>
          <select
            aria-label="Wrong label?"
            value=""
            onChange={(e) => e.target.value && setRelabel(e.target.value)}
            className="h-8 rounded-(--radius-control) border border-border bg-(--input-bg) px-2 text-sm"
          >
            <option value="">Wrong label?</option>
            {LABELS.filter((x) => x !== m.label).map((x) => (
              <option key={x} value={x}>
                {say("label", x)}
              </option>
            ))}
          </select>
        </div>
      </header>
      {m.bounce_diagnostic && (
        <p className="rounded bg-danger/10 p-2 text-sm text-danger">{m.bounce_diagnostic}</p>
      )}
      <pre className="whitespace-pre-wrap font-sans text-sm">{m.reply_text}</pre>
      {job.data && (
        <div className="rounded-(--radius-control) bg-surface-2 p-3 text-sm">
          <p className="text-xs text-muted">
            The email it answered
            {job.data.job.step_number ? ` (step ${job.data.job.step_number})` : ""}
          </p>
          <p className="font-medium">{job.data.job.subject}</p>
          <p className="line-clamp-3 whitespace-pre-wrap text-muted">{job.data.job.body_text}</p>
        </div>
      )}
      {l && (
        <div className="flex flex-wrap items-center justify-between gap-3 border-t border-border pt-3 text-sm">
          <Link to="/w/$ws/leads" params={{ ws }} search={{ lead: l.id }} className="underline">
            {[l.first_name, l.last_name].filter(Boolean).join(" ") || l.email}
            {l.company_name ? `, ${l.company_name}` : ""}
          </Link>
          <div className="flex flex-wrap gap-1" role="group" aria-label="Interest">
            {INTERESTS.map((i) => (
              <button
                key={i}
                type="button"
                aria-pressed={l.interest === i}
                onClick={() => interest.mutate(i)}
                className={cn(
                  "rounded-full border px-2.5 py-0.5 text-xs",
                  l.interest === i ? "border-accent bg-accent-soft font-medium" : "border-border",
                )}
              >
                {say("interest", i)}
              </button>
            ))}
          </div>
        </div>
      )}
      {m.label === "reply" && (
        <div className="flex flex-col gap-2 border-t border-border pt-3">
          <TextArea
            label="Your reply"
            value={reply}
            onChange={(e) => setReply(e.target.value)}
            rows={5}
          />
          <p className="text-xs text-muted">
            Goes out from {account?.email ?? "the same account"}, in the same thread, at the next
            free slot (usually a few minutes inside sending hours).
          </p>
          <div>
            <Button disabled={!reply.trim()} busy={send.isPending} onClick={() => send.mutate()}>
              Send reply
            </Button>
          </div>
        </div>
      )}
      <ConfirmDialog
        open={!!relabel}
        onClose={() => setRelabel(null)}
        danger={false}
        confirmLabel="Change label"
        title="Change the label?"
        text="This changes what we blocked or stopped."
        onConfirm={async () => {
          await http.patch("/v1/replies/messages/{id}", {
            path: { id: m.id },
            body: { label: relabel },
          });
          await qc.invalidateQueries({ queryKey: ["inbox"] });
        }}
      />
    </article>
  );
}
