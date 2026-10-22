import { useInfiniteQuery, useQuery } from "@tanstack/react-query";
import { type ReactNode, useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { Button, ErrorNote, Field, Modal, Notice, Pill, Select, Skeleton } from "@/components/ui";
import { cn } from "@/lib/cn";
import { fullName } from "@/lib/format";
import { sanitizeHtml } from "@/lib/html";
import { required } from "@/lib/template";

type Mail = {
  step_number: number;
  delay_amount: number;
  delay_type: string;
  subject: string;
  body_text?: string | null;
  body_html?: string | null;
  /** `saved`, `template` or `not_written` */
  source: string;
  /** Names the lead has no value for. They stay in the text as `{{name}}`. */
  missing: string[];
};

type Reply = {
  from_name: string;
  from_email: string;
  to_name: string;
  to_email: string;
  steps: Mail[];
};

const SAMPLE = "sample";
const BUILT_IN = ["first_name", "last_name", "company_name"];

/** Wrap every `{{name}}` of the missing names, so the person sees what has no value. */
function marked(text: string, names: string[], wrap: (m: string, i: number) => ReactNode): ReactNode[] {
  if (!names.length) return [text];
  const re = new RegExp(`(\\{\\{(?:${names.join("|")})\\}\\})`, "g");
  return text.split(re).map((part, i) => (i % 2 === 1 ? wrap(part, i) : part));
}

const MARK = "rounded bg-warning/25 px-0.5 text-text";

function Highlighted({ text, names }: { text: string; names: string[] }) {
  return <>{marked(text, names, (m, i) => <mark key={i} className={MARK}>{m}</mark>)}</>;
}

function htmlDoc(html: string, names: string[]) {
  let body = sanitizeHtml(html);
  if (names.length) {
    body = body.replace(
      new RegExp(`\\{\\{(?:${names.join("|")})\\}\\}`, "g"),
      (m) => `<mark style="background:#fde68a;border-radius:3px;padding:0 2px">${m}</mark>`,
    );
  }
  return `<base target="_blank"><style>body{margin:0;padding:4px 2px;font:14px/1.55 -apple-system,Segoe UI,Roboto,Helvetica,Arial,sans-serif;color:#202124}p{margin:0 0 .9em}a{color:#1a0dab}</style>${body}`;
}

function stepName(n: number) {
  return n === 1 ? "Email 1" : `Follow-up ${n - 1}`;
}

/** One email, drawn like a message in a mailbox. */
function Message({ mail, reply }: { mail: Mail; reply: Reply }) {
  const sender = reply.from_name || reply.from_email;
  const to = reply.to_name ? `${reply.to_name} <${reply.to_email}>` : reply.to_email;
  const notWritten = mail.source === "not_written";
  return (
    <article className="overflow-hidden rounded-(--radius-card) border border-border bg-surface">
      <header className="border-b border-border px-5 py-4">
        <h3 className="text-lg font-semibold leading-snug break-words">
          {notWritten ? (
            <span className="text-muted">(not written yet)</span>
          ) : mail.subject ? (
            <Highlighted text={mail.subject} names={mail.missing} />
          ) : (
            <span className="text-muted">(no subject)</span>
          )}
        </h3>
        <div className="mt-3 flex items-start gap-3">
          <span
            aria-hidden="true"
            className="flex size-9 shrink-0 items-center justify-center rounded-full bg-accent-soft text-sm font-semibold text-accent"
          >
            {sender.slice(0, 1).toUpperCase()}
          </span>
          <div className="min-w-0 flex-1 text-sm">
            <p className="truncate">
              <span className="font-medium">{reply.from_name}</span>{" "}
              <span className="text-muted">&lt;{reply.from_email}&gt;</span>
            </p>
            <p className="truncate text-muted">to {to}</p>
          </div>
          <div className="shrink-0 text-right text-xs text-muted">
            <p>{stepName(mail.step_number)}</p>
            <p>
              {mail.step_number === 1
                ? "Sent first"
                : `after ${mail.delay_amount} ${mail.delay_type === "business_days" ? "business" : "calendar"} days`}
            </p>
          </div>
        </div>
      </header>
      <div className="bg-white px-5 py-4 text-sm text-neutral-900">
        {notWritten ? (
          <p className="text-neutral-500">
            The AI has not written this email for this lead yet. Use Generate in the Leads tab to write it.
          </p>
        ) : mail.body_html ? (
          <iframe
            title={`${stepName(mail.step_number)} body`}
            sandbox=""
            srcDoc={htmlDoc(mail.body_html, mail.missing)}
            className="h-80 w-full border-0 bg-white"
          />
        ) : (
          <div className="whitespace-pre-wrap break-words leading-relaxed">
            <Highlighted text={mail.body_text ?? ""} names={mail.missing} />
          </div>
        )}
      </div>
    </article>
  );
}

/**
 * A popup with the leads of the campaign on the left. Pick one and see every email of the sequence
 * the way that lead gets it, in a mailbox view, with the lead's own values filled in.
 */
export function PreviewDrawer({
  open,
  onClose,
  campaignId,
  accounts,
  userEmail,
  texts,
}: {
  open: boolean;
  onClose: () => void;
  campaignId: string;
  accounts: { id: string; email: string }[];
  userEmail: string;
  texts: string[];
}) {
  const [filter, setFilter] = useState("");
  const [pick, setPick] = useState<string>("");
  const [step, setStep] = useState(0);
  const [account, setAccount] = useState(accounts[0]?.id ?? "");
  const [sample, setSample] = useState<Record<string, string>>({
    first_name: "Ana",
    last_name: "Smith",
    company_name: "Acme",
  });
  const [testTo, setTestTo] = useState(userEmail);
  const [sending, setSending] = useState(false);

  const leads = useInfiniteQuery({
    queryKey: ["campaigns", campaignId, "preview-leads"],
    enabled: open,
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      http.get<S["CampaignLeadsReply"]>("/v1/campaigns/{id}/leads", {
        path: { id: campaignId },
        query: { limit: 50, cursor: pageParam },
      }),
    getNextPageParam: (l) => l.next_cursor ?? undefined,
  });
  const rows = useMemo(() => leads.data?.pages.flatMap((p) => p.leads) ?? [], [leads.data]);
  const shown = useMemo(() => {
    const f = filter.trim().toLowerCase();
    if (!f) return rows;
    return rows.filter((l) => `${fullName(l)} ${l.email} ${l.company_name ?? ""}`.toLowerCase().includes(f));
  }, [rows, filter]);

  // the first lead is open when the popup opens; with no leads there are sample values
  useEffect(() => {
    if (!open || pick || leads.isPending) return;
    setPick(rows[0]?.lead_id ?? SAMPLE);
  }, [open, pick, rows, leads.isPending]);
  useEffect(() => {
    if (!account && accounts[0]) setAccount(accounts[0].id);
  }, [accounts, account]);

  const extra = useMemo(
    () =>
      [
        ...new Set(
          texts.flatMap((t) => {
            try {
              return required(t);
            } catch {
              return [];
            }
          }),
        ),
      ].filter((n) => !BUILT_IN.includes(n)),
    [texts],
  );

  const isSample = pick === SAMPLE;
  const sampleKey = JSON.stringify(sample);
  const preview = useQuery({
    queryKey: ["campaigns", campaignId, "preview", pick, account, isSample ? sampleKey : ""],
    enabled: open && !!pick,
    staleTime: 0,
    placeholderData: (prev) => prev,
    queryFn: () =>
      http.post<Reply>("/v1/campaigns/{id}/emails/preview", {
        path: { id: campaignId },
        body: isSample
          ? { variables: sample, account_id: account || null }
          : { lead_id: pick, account_id: account || null },
      }),
  });
  const mails = preview.data?.steps ?? [];
  const cur = mails[Math.min(step, Math.max(mails.length - 1, 0))];
  const missingAll = [...new Set(mails.flatMap((m) => m.missing))];

  async function sendTest() {
    if (!cur || !account) return;
    setSending(true);
    try {
      const r = await http.post<S["SendTestReply"]>("/v1/campaigns/{id}/emails/send-test", {
        path: { id: campaignId },
        body: {
          to: testTo,
          account_id: account,
          lead_id: isSample ? null : pick,
          subject: cur.subject,
          body_text: cur.body_text ?? null,
          body_html: cur.body_html ?? null,
        },
      });
      toast.success(`Sent to ${r.to}. Check your inbox.`);
    } catch (e) {
      toast.error(messageOf(e));
    } finally {
      setSending(false);
    }
  }

  return (
    <Modal open={open} onClose={onClose} size="xl" title="Preview">
      <div className="grid gap-4 md:grid-cols-[17rem_minmax(0,1fr)]">
        <aside className="flex min-h-0 flex-col gap-2 md:max-h-[70vh]">
          <Field
            label="Leads of this campaign"
            placeholder="Search by name or email"
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
          />
          <ul className="flex min-h-0 flex-1 flex-col gap-1 overflow-auto" aria-label="Leads">
            <li>
              <LeadButton
                active={isSample}
                onClick={() => setPick(SAMPLE)}
                title="Sample values"
                sub="Type your own values"
              />
            </li>
            {leads.isPending && (
              <li>
                <Skeleton />
              </li>
            )}
            {shown.map((l) => (
              <li key={l.lead_id}>
                <LeadButton
                  active={pick === l.lead_id}
                  onClick={() => setPick(l.lead_id)}
                  title={fullName(l) || l.email}
                  sub={fullName(l) ? l.email : (l.company_name ?? "")}
                  warn={l.missing.length > 0 ? `No value for: ${l.missing.join(", ")}` : undefined}
                />
              </li>
            ))}
            {!leads.isPending && rows.length === 0 && (
              <li className="px-1 text-xs text-muted">
                No leads in this campaign yet. Add leads to preview the emails with real values.
              </li>
            )}
            {!leads.isPending && rows.length > 0 && shown.length === 0 && (
              <li className="px-1 text-xs text-muted">No loaded lead matches.</li>
            )}
            {leads.hasNextPage && (
              <li>
                <Button
                  variant="secondary"
                  className="w-full"
                  busy={leads.isFetchingNextPage}
                  onClick={() => leads.fetchNextPage()}
                >
                  Load more leads
                </Button>
              </li>
            )}
          </ul>
        </aside>

        <section className="flex min-w-0 flex-col gap-3">
          <Select
            label="Send from"
            value={account}
            onChange={setAccount}
            options={
              accounts.length
                ? accounts.map((a) => ({ value: a.id, label: a.email }))
                : [{ value: "", label: "No account chosen yet" }]
            }
          />
          {isSample && (
            <div className="grid gap-2 sm:grid-cols-2">
              {[...BUILT_IN, ...extra].map((k) => (
                <Field
                  key={k}
                  label={k}
                  value={sample[k] ?? ""}
                  onChange={(e) => setSample({ ...sample, [k]: e.target.value })}
                />
              ))}
            </div>
          )}
          {preview.isError && <ErrorNote error={preview.error} />}
          {preview.isPending && pick && <Skeleton />}
          {mails.length > 1 && (
            <div role="tablist" aria-label="Emails of the sequence" className="flex flex-wrap gap-1">
              {mails.map((m, i) => (
                <button
                  key={m.step_number}
                  type="button"
                  role="tab"
                  aria-selected={cur?.step_number === m.step_number}
                  onClick={() => setStep(i)}
                  className={cn(
                    "rounded-full border px-3 py-1 text-sm",
                    cur?.step_number === m.step_number
                      ? "border-accent bg-accent-soft font-medium text-accent"
                      : "border-border text-muted hover:bg-surface-2",
                  )}
                >
                  {stepName(m.step_number)}
                  {m.missing.length > 0 && <span className="ml-1 text-warning">•</span>}
                </button>
              ))}
            </div>
          )}
          {missingAll.length > 0 && (
            <Pill tone="warning">
              {isSample ? "No sample value yet" : "This lead has no value"} for {missingAll.join(", ")}. It shows highlighted.
            </Pill>
          )}
          {cur && preview.data && <Message mail={cur} reply={preview.data} />}
          {cur && (
            <div className="flex flex-col gap-2 border-t border-border pt-3">
              <Field
                label={`Send ${stepName(cur.step_number)} as a test to`}
                value={testTo}
                onChange={(e) => setTestTo(e.target.value)}
              />
              <div>
                <Button
                  variant="secondary"
                  busy={sending}
                  onClick={sendTest}
                  disabled={!account || !testTo || cur.source === "not_written"}
                >
                  Send to me
                </Button>
              </div>
              <Notice>A test goes out now. It skips the queue, sending hours and limits.</Notice>
            </div>
          )}
        </section>
      </div>
    </Modal>
  );
}

function LeadButton({
  active,
  onClick,
  title,
  sub,
  warn,
}: {
  active: boolean;
  onClick: () => void;
  title: string;
  sub: string;
  warn?: string;
}) {
  return (
    <button
      type="button"
      onClick={onClick}
      aria-current={active}
      title={warn}
      className={cn(
        "flex w-full items-center gap-2 rounded-(--radius-control) border px-2.5 py-1.5 text-left text-sm",
        active ? "border-accent bg-accent-soft" : "border-transparent hover:bg-surface-2",
      )}
    >
      <span className="min-w-0 flex-1">
        <span className="block truncate font-medium">{title}</span>
        {sub && <span className="block truncate text-xs text-muted">{sub}</span>}
      </span>
      {warn && <span aria-label={warn} className="size-2 shrink-0 rounded-full bg-warning" />}
    </button>
  );
}
