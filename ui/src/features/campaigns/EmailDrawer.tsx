import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { ApiProblem, messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { Button, ErrorNote, Field, Modal, Pill, Skeleton, TextArea } from "@/components/ui";
import { fmtDateTime } from "@/lib/format";
import { EmailScore } from "./EmailScore";

export const EMAIL_STATUS: Record<string, { word: string; tone: "neutral" | "success" | "warning" | "danger" | "accent" }> = {
  none: { word: "Not written", tone: "neutral" },
  writing: { word: "Writing...", tone: "accent" },
  review: { word: "Needs review", tone: "warning" },
  ready: { word: "Ready", tone: "success" },
  stale: { word: "Out of date", tone: "warning" },
  failed: { word: "Failed", tone: "danger" },
  held: { word: "Held", tone: "danger" },
  sent: { word: "Sent", tone: "neutral" },
};

export const stepName = (n: number) => (n === 1 ? "Primary email" : `Follow up ${n - 1}`);

export function StatusChip({ status, hint }: { status: string; hint?: string | null | undefined }) {
  const s = EMAIL_STATUS[status] ?? { word: status, tone: "neutral" as const };
  return (
    <span title={hint ?? undefined}>
      <Pill tone={s.tone}>{s.word}</Pill>
    </span>
  );
}

/** One saved email: read it, edit it by hand. */
export function EmailDrawer({
  campaignId,
  leadId,
  email,
  step,
  onClose,
}: {
  campaignId: string;
  leadId: string | null;
  email: string;
  step: number;
  onClose: () => void;
}) {
  const qc = useQueryClient();
  const path = { id: campaignId, lead_id: leadId ?? "", step };
  const q = useQuery({
    queryKey: ["campaigns", campaignId, "email", leadId, step],
    enabled: !!leadId,
    retry: false,
    queryFn: () =>
      http.get<S["LeadEmail"]>("/v1/campaigns/{id}/leads/{lead_id}/emails/{step}", { path }),
  });
  const missing = q.error instanceof ApiProblem && q.error.code === "email_not_found";
  const e = q.data;
  const [subject, setSubject] = useState("");
  const [bodyText, setBodyText] = useState("");
  useEffect(() => {
    setSubject(e?.subject ?? "");
    setBodyText(e?.body_text ?? "");
  }, [e]);
  const sent = e?.status === "sent";
  const save = useMutation({
    mutationFn: () =>
      http.patch<S["LeadEmail"]>("/v1/campaigns/{id}/leads/{lead_id}/emails/{step}", {
        path,
        body: { subject, body_text: bodyText },
      }),
    onSuccess: () => {
      toast.success("Saved. This email is kept when you Save again.");
      void qc.invalidateQueries({ queryKey: ["campaigns", campaignId] });
    },
    onError: (err) => toast.error(messageOf(err)),
  });
  const act = useMutation({
    mutationFn: (action: "approve" | "regenerate" | "clear" | "skip") =>
      http.post<S["ActionReply"]>("/v1/campaigns/{id}/emails/actions", { path: { id: campaignId }, body: { action, ids: [e?.id ?? ""] } }),
    onSuccess: (_r, action) => {
      toast.success(action === "approve" ? "Approved." : action === "clear" ? "Cleared." : action === "skip" ? "Lead skipped." : "Writing it again.");
      void qc.invalidateQueries({ queryKey: ["campaigns", campaignId] });
      if (action === "clear" || action === "skip") onClose();
    },
    onError: (err) => toast.error(messageOf(err)),
  });
  const words = bodyText.trim() ? bodyText.trim().split(/\s+/).length : 0;
  return (
    <Modal
      open={!!leadId}
      onClose={onClose}
      side
      title={`${stepName(step)} for ${email}`}
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Close
          </Button>
          {e && !sent && e.status === "review" && (
            <Button variant="secondary" busy={act.isPending} onClick={() => act.mutate("approve")}>
              Approve
            </Button>
          )}
          {e && e.status === "held" && (
            <Button variant="secondary" busy={act.isPending} onClick={() => act.mutate("skip")}>
              Skip this lead
            </Button>
          )}
          {e && !sent && e.source === "ai" && (
            <Button variant="secondary" busy={act.isPending} onClick={() => act.mutate("regenerate")}>
              {e.status === "held" ? "Edit lead and retry" : "Write again"}
            </Button>
          )}
          {e && !sent && (
            <Button variant="ghost" busy={act.isPending} onClick={() => act.mutate("clear")}>
              Clear
            </Button>
          )}
          {!sent && (
            <Button busy={save.isPending} onClick={() => save.mutate()}>
              Save by hand
            </Button>
          )}
        </>
      }
    >
      {q.isPending && leadId && <Skeleton rows={4} />}
      {q.isError && !missing && <ErrorNote error={q.error} />}
      {(e || missing) && (
        <div className="flex flex-col gap-3">
          <div className="flex flex-wrap items-center gap-2 text-sm">
            <StatusChip status={e?.status ?? "none"} hint={e?.error_code} />
            {e?.variant_key && <Pill>Variant {e.variant_key}</Pill>}
            {e?.edited_by_hand && <Pill tone="accent">Edited</Pill>}
            {sent && <span className="text-muted">Sent {fmtDateTime(e.updated_at)}</span>}
          </div>
          {e?.status === "stale" && (
            <p className="text-sm text-muted">
              The lead or the template changed after this email was saved. It is sent as written.
              Save the template again to write it fresh.
            </p>
          )}
          {e?.error_code && <p className="text-sm text-danger">{e.error_code}</p>}
          {e?.status === "held" && (
            <div className="rounded-(--radius-control) border border-danger/40 bg-danger/10 p-3 text-sm" role="note">
              <p className="font-medium">This email is held and will not be sent.</p>
              <p>{e.hold_reason}</p>
              {Array.isArray(e.check_failures) && e.check_failures.length > 0 && (
                <ul className="mt-1 list-disc pl-5">
                  {(e.check_failures as string[]).map((f) => (
                    <li key={f}>{f}</li>
                  ))}
                </ul>
              )}
              <p className="mt-1 text-muted">Fix the lead (or your PERSONALIZATION page), then press "Edit lead and retry".</p>
            </div>
          )}
          {missing && <p className="text-sm text-muted">No email is written yet. Save the template, or write one here.</p>}
          {step === 1 || subject ? (
            <Field label="Subject" value={subject} disabled={sent} onChange={(ev) => setSubject(ev.target.value)} />
          ) : (
            <p className="text-sm text-muted">Sent in the same thread (Re: + the first subject).</p>
          )}
          <EmailScore text={bodyText} />
          <TextArea
            label="Body"
            rows={14}
            value={bodyText}
            disabled={sent}
            onChange={(ev) => setBodyText(ev.target.value)}
            help={`${words} words. The tokens {{sender_name}} and {{sender_email}} are filled when the email is sent.`}
          />
        </div>
      )}
    </Modal>
  );
}
