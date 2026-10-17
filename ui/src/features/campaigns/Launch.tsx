import { useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { ApiProblem, messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import type { SendStatus } from "@/api/queries";
import { Button, Modal, Notice } from "@/components/ui";
import { fmtDateTime, fmtInt } from "@/lib/format";
import { PROBLEM_WORDS } from "@/lib/words";

/** Show what will happen, then start the campaign. A refused launch lists each problem in words. */
export function LaunchDialog({
  open,
  onClose,
  campaign,
  ws,
}: {
  open: boolean;
  onClose: () => void;
  campaign: S["CampaignDetail"];
  ws: string;
}) {
  const qc = useQueryClient();
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<ApiProblem | string | null>(null);
  const [first, setFirst] = useState<{ at: string; tz: string } | null>(null);
  useEffect(() => {
    if (!open) return;
    setProblem(null);
    setFirst(null);
    Promise.all(
      campaign.accounts.map((a) =>
        http
          .get<SendStatus>("/v1/sending/accounts/{id}/status", { path: { id: a.id } })
          .catch(() => null),
      ),
    ).then((all) => {
      const times = all
        .filter((s): s is SendStatus => !!s && !!(s.next_send_at ?? s.wait?.until))
        .map((s) => ({ at: (s.next_send_at ?? s.wait?.until) as string, tz: s.timezone }));
      times.sort((a, b) => a.at.localeCompare(b.at));
      setFirst(times[0] ?? null);
    });
  }, [open, campaign.accounts]);

  async function launch() {
    setBusy(true);
    setProblem(null);
    try {
      await http.post("/v1/campaigns/{id}/launch", { path: { id: campaign.id } });
      await qc.invalidateQueries({ queryKey: ["campaigns"] });
      onClose();
    } catch (e) {
      setProblem(e instanceof ApiProblem ? e : messageOf(e));
    } finally {
      setBusy(false);
    }
  }
  const p = typeof problem === "string" ? null : problem;
  const fix =
    p?.code === "missing_variables" ? (
      <Link to="/w/$ws/leads" params={{ ws }} search={{ tab: "fields" }} className="underline">
        Fill or add a fallback
      </Link>
    ) : p?.code === "no_accounts" || p?.code === "no_healthy_account" ? (
      <Link
        to="/w/$ws/campaigns/$id/settings"
        params={{ ws, id: campaign.id }}
        className="underline"
      >
        Choose accounts
      </Link>
    ) : p?.code === "generator_not_ready" ? (
      <Link to="/w/$ws/settings/integrations/$id" params={{ ws, id: "ai" }} className="underline">
        Set up an AI model
      </Link>
    ) : p?.code === "no_steps" ? (
      <Link to="/w/$ws/campaigns/$id/editor" params={{ ws, id: campaign.id }} className="underline">
        Write an email
      </Link>
    ) : null;
  return (
    <Modal
      open={open}
      onClose={onClose}
      title="Launch this campaign"
      size="md"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button busy={busy} onClick={launch}>
            Launch campaign
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        <dl className="grid grid-cols-2 gap-y-1.5 text-sm">
          <dt className="text-muted">Leads waiting</dt>
          <dd>{fmtInt(campaign.counts.pending)}</dd>
          <dt className="text-muted">Accounts</dt>
          <dd>{campaign.accounts.length}</dd>
          <dt className="text-muted">New leads per day</dt>
          <dd>{fmtInt(campaign.daily_new_leads)}</dd>
          <dt className="text-muted">Emails in the sequence</dt>
          <dd>{campaign.steps.length}</dd>
          <dt className="text-muted">First email goes out</dt>
          <dd>
            {first
              ? `${fmtDateTime(first.at, first.tz)} (${first.tz})`
              : "at the next sending time"}
          </dd>
        </dl>
        {campaign.warnings.map((w) => (
          <Notice key={w} tone="warning">
            {w}
          </Notice>
        ))}
        {problem && (
          <Notice tone="danger">
            <p>{p ? (PROBLEM_WORDS[p.code] ?? p.message) : String(problem)}</p>
            {p?.code === "missing_variables" && <p className="mt-1">{p.message}</p>}
            {fix && <p className="mt-1">{fix}</p>}
          </Notice>
        )}
      </div>
    </Modal>
  );
}
