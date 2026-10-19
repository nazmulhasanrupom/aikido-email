import { useQuery } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import {
  Button,
  ErrorNote,
  Field,
  Modal,
  Notice,
  Pill,
  Select,
  useDebounced,
} from "@/components/ui";
import { fullName } from "@/lib/format";
import { sanitizeHtml } from "@/lib/html";
import { required } from "@/lib/template";

type Step = {
  subject: string;
  body_text?: string | null;
  body_html?: string | null;
  step_number: number;
  delay_amount: number;
  delay_type: string;
};

/** See each email of the sequence for one lead (or sample values), and send a test to yourself. */
export function PreviewDrawer({
  open,
  onClose,
  sequenceId,
  campaignId,
  accounts,
  userEmail,
  texts,
}: {
  open: boolean;
  onClose: () => void;
  sequenceId: string;
  campaignId: string;
  accounts: { id: string; email: string }[];
  userEmail: string;
  texts: string[];
}) {
  const [leadQ, setLeadQ] = useState("");
  const dq = useDebounced(leadQ, 250);
  const [lead, setLead] = useState<S["Lead"] | null>(null);
  const [account, setAccount] = useState(accounts[0]?.id ?? "");
  const [sample, setSample] = useState<Record<string, string>>({
    first_name: "Ana",
    last_name: "Smith",
    company_name: "Acme",
  });
  const [steps, setSteps] = useState<Step[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const [testTo, setTestTo] = useState(userEmail);
  const found = useQuery({
    queryKey: ["leads", "pick", campaignId, dq],
    enabled: open && dq.length >= 2,
    queryFn: () =>
      http.get<S["ListLeadsReply"]>("/v1/leads", {
        query: { q: dq, campaign_id: campaignId, limit: 5 },
      }),
  });
  useEffect(() => {
    if (!account && accounts[0]) setAccount(accounts[0].id);
  }, [accounts, account]);
  const extra = [
    ...new Set(
      texts.flatMap((t) => {
        try {
          return required(t);
        } catch {
          return [];
        }
      }),
    ),
  ].filter((n) => !["first_name", "last_name", "company_name"].includes(n));

  async function run(generate: boolean) {
    setBusy(true);
    setError(null);
    try {
      const body = lead
        ? { lead_id: lead.id, account_id: account || null, generate }
        : { to_email: "sample@example.com", variables: sample, account_id: account || null };
      const r = await http.post<{ steps: Step[] }>("/v1/followups/sequences/{id}/preview", {
        path: { id: sequenceId },
        body,
      });
      setSteps(r.steps);
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  async function sendTest() {
    const first = steps?.[0];
    if (!first || !account) return;
    try {
      await http.post<{ jobs: S["Job"][] }>("/v1/sending/jobs", {
        body: {
          jobs: [
            {
              account_id: account,
              to_email: testTo,
              subject: first.subject,
              body_text: first.body_text ?? "",
              body_html: first.body_html ?? null,
              kind: "manual",
              priority: 100,
              metadata: { test: true },
            },
          ],
        },
      });
      toast.success("Test sent. It arrives in a minute or two.");
    } catch (e) {
      toast.error(messageOf(e));
    }
  }

  return (
    <Modal open={open} onClose={onClose} side title="Preview">
      <div className="flex flex-col gap-4">
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
        <div className="flex flex-col gap-2">
          <Field
            label="Pick a lead of this campaign"
            placeholder="Search by name or email"
            value={lead ? fullName(lead) || lead.email : leadQ}
            onChange={(e) => {
              setLead(null);
              setLeadQ(e.target.value);
            }}
          />
          {!lead &&
            found.data?.leads.map((l) => (
              <button
                key={l.id}
                type="button"
                className="rounded border border-border px-2 py-1 text-left text-sm hover:bg-surface-2"
                onClick={() => setLead(l)}
              >
                {fullName(l)} <span className="text-muted">{l.email}</span>
              </button>
            ))}
          {lead && (
            <button
              type="button"
              className="self-start text-sm text-accent underline"
              onClick={() => setLead(null)}
            >
              Use sample values instead
            </button>
          )}
        </div>
        {!lead && (
          <div className="grid grid-cols-2 gap-2">
            {["first_name", "last_name", "company_name", ...extra].map((k) => (
              <Field
                key={k}
                label={k}
                value={sample[k] ?? ""}
                onChange={(e) => setSample({ ...sample, [k]: e.target.value })}
              />
            ))}
          </div>
        )}
        <div className="flex flex-wrap gap-2">
          <Button busy={busy} onClick={() => run(false)}>
            Show emails
          </Button>
          {lead && (
            <Button variant="secondary" onClick={() => run(true)} title="Uses AI tokens">
              Write AI parts for this lead
            </Button>
          )}
        </div>
        {lead && <p className="text-xs text-muted">Writing AI parts uses AI tokens.</p>}
        {error != null && <ErrorNote error={error} />}
        {steps?.map((s) => (
          <article
            key={s.step_number}
            className="rounded-(--radius-card) border border-border p-3 text-sm"
          >
            <div className="mb-2 flex items-center justify-between text-xs text-muted">
              <Pill>{s.step_number === 1 ? "Email 1" : `Follow-up ${s.step_number - 1}`}</Pill>
              {s.step_number > 1 && (
                <span>
                  after {s.delay_amount}{" "}
                  {s.delay_type === "business_days" ? "business" : "calendar"} days
                </span>
              )}
            </div>
            <p className="text-muted">
              From: {accounts.find((a) => a.id === account)?.email ?? ""}
            </p>
            <p className="font-medium">{s.subject}</p>
            {s.body_html ? (
              <iframe
                title={`Email ${s.step_number}`}
                sandbox=""
                srcDoc={sanitizeHtml(s.body_html)}
                className="mt-2 h-48 w-full rounded border border-border bg-white"
              />
            ) : (
              <pre className="mt-2 whitespace-pre-wrap font-sans">{s.body_text}</pre>
            )}
          </article>
        ))}
        {steps && (
          <div className="flex flex-col gap-2 border-t border-border pt-3">
            <Field
              label="Send the first email as a test to"
              value={testTo}
              onChange={(e) => setTestTo(e.target.value)}
            />
            <div>
              <Button variant="secondary" onClick={sendTest} disabled={!account || !testTo}>
                Send to me
              </Button>
            </div>
            <Notice>A test goes out now. Sending hours and limits do not apply.</Notice>
          </div>
        )}
      </div>
    </Modal>
  );
}
