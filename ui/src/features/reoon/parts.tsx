import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http } from "@/api/http";
import { Button, Pill } from "@/components/ui";
import { fmtAgo, fmtDateTime } from "@/lib/format";
import { say } from "@/lib/words";
import { FLAG_WORDS, type Result, statusTone, useReoon } from "./api";

/** The verification status as a coloured pill. The score and the date are in the tooltip. */
export function VerifyPill({
  status,
  score,
  checkedAt,
}: {
  status: string | null | undefined;
  score?: number | null | undefined;
  checkedAt?: string | null | undefined;
}) {
  const code = status || "none";
  const tip = status
    ? [
        score != null ? `Score ${score} of 100` : null,
        checkedAt ? `Checked ${fmtDateTime(checkedAt)} (${fmtAgo(checkedAt)})` : null,
      ]
        .filter(Boolean)
        .join(". ")
    : "Not verified yet";
  return (
    <span title={tip}>
      <Pill tone={statusTone(code)}>{say("reoon", code)}</Pill>
    </span>
  );
}

/** The Verification block of the lead drawer: status, score, flags in plain words, and "Verify now". */
export function VerifyBlock({ leadId }: { leadId: string }) {
  const qc = useQueryClient();
  const reoon = useReoon();
  const result = useQuery({
    queryKey: ["reoon", "lead", leadId],
    enabled: reoon.installed,
    queryFn: () => http.get<Result | null>("/v1/verify/reoon/leads/{id}", { path: { id: leadId } }),
    refetchInterval: (query) => (query.state.data === undefined ? false : 10_000),
  });
  const verify = useMutation({
    mutationFn: () => http.post("/v1/verify/reoon/runs", { body: { lead_ids: [leadId], force: true } }),
    onSuccess: () => {
      toast.success("Checking this email now.");
      void qc.invalidateQueries({ queryKey: ["reoon"] });
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  if (!reoon.installed) return null;
  const r = result.data;
  const flags = r
    ? Object.entries(FLAG_WORDS).flatMap(([key, [yes, no]]) => {
        const v = r.flags[key];
        const text = v === true ? yes : v === false ? no : "";
        return text ? [text] : [];
      })
    : [];
  return (
    <section aria-label="Verification" className="flex flex-col gap-2 rounded-(--radius-card) border border-border p-3 sm:col-span-2">
      <div className="flex flex-wrap items-center gap-2">
        <h3 className="text-sm font-semibold">Verification</h3>
        <VerifyPill status={r?.status} score={r?.score} checkedAt={r?.checked_at} />
        {r?.score != null && <span className="text-sm text-muted">Score {r.score}</span>}
        {r && <span className="text-sm text-muted">Checked {fmtAgo(r.checked_at)}</span>}
        <Button variant="secondary" className="ml-auto h-7" busy={verify.isPending} onClick={() => verify.mutate()}>
          Verify now
        </Button>
      </div>
      {flags.length > 0 && <p className="text-sm text-muted">{flags.join(". ")}.</p>}
    </section>
  );
}
