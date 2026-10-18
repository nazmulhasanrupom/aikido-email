import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import { Button, Card, ConfirmDialog, Notice, Page, PageHeader, Pill } from "@/components/ui";
import { useReoon } from "@/features/reoon/api";
import { useUser } from "@/features/settings/useUser";
import { fmtAgo } from "@/lib/format";

export const Route = createFileRoute("/w/$ws/status")({ component: Status });

interface Part {
  id: string;
  title: string;
  base: string;
  statusPath: string;
  keeps: string;
}
const PARTS: Part[] = [
  {
    id: "sending",
    title: "Sending",
    base: "/v1/sending/engine",
    statusPath: "/v1/sending/engine/status",
    keeps: "Emails stop going out. Nothing is lost; they wait in the queue.",
  },
  {
    id: "replies",
    title: "Reply check",
    base: "/v1/replies/engine",
    statusPath: "/v1/replies/engine/status",
    keeps: "Inboxes are not read. Follow-ups wait for the inbox check.",
  },
  {
    id: "followups",
    title: "Follow-ups",
    base: "/v1/followups",
    statusPath: "/v1/followups/status",
    keeps: "Emails already in the queue still go out.",
  },
  {
    id: "campaigns",
    title: "Campaign feeder",
    base: "/v1/campaigns/feeder",
    statusPath: "/v1/campaigns/status",
    keeps: "No new leads join a sequence. Running sequences go on.",
  },
  {
    id: "generator",
    title: "AI writer",
    base: "/v1/generator",
    statusPath: "/v1/generator/status",
    keeps: "No new AI texts are written. Ready texts are still used.",
  },
];

const VERIFIER: Part = {
  id: "verifier",
  title: "Email verifier",
  base: "/v1/verify/reoon",
  statusPath: "/v1/verify/reoon/status",
  keeps: "Emails are not checked. Runs wait until you resume.",
};

function Status() {
  const { isAdmin } = useUser();
  const health = useQuery({
    queryKey: ["health"],
    queryFn: () => http.get<S["Health"]>("/health"),
    refetchInterval: 15_000,
  });
  const cfg = useQuery(q.config);
  const reoon = useReoon();
  return (
    <Page>
      <PageHeader title="Status" sub="Is everything running?" />
      <div className="grid gap-4 md:grid-cols-2">
        <Card className="flex flex-col gap-1.5">
          <div className="flex items-center justify-between">
            <h2 className="font-semibold">Server</h2>
            <Pill tone={health.data?.status === "ok" ? "success" : "danger"}>
              {health.data?.status === "ok" ? "Running" : health.isError ? "Not answering" : "..."}
            </Pill>
          </div>
          <p className="text-sm text-muted">Version {cfg.data?.version ?? "..."}</p>
          {cfg.data?.dry_run && (
            <Notice tone="warning">Dry run: emails are logged, not sent.</Notice>
          )}
        </Card>
        {PARTS.map((p) => (
          <PartCard key={p.id} part={p} admin={isAdmin} />
        ))}
        {reoon.installed && <PartCard part={VERIFIER} admin={isAdmin} />}
      </div>
    </Page>
  );
}

interface Generic {
  paused: boolean;
  [k: string]: unknown;
}

function PartCard({ part, admin }: { part: Part; admin: boolean }) {
  const qc = useQueryClient();
  const res = useQuery({
    queryKey: ["status", part.id],
    queryFn: () => http.get<Generic>(part.statusPath),
    refetchInterval: 15_000,
  });
  const [ask, setAsk] = useState(false);
  const toggle = useMutation({
    mutationFn: () =>
      http.post(`${part.base}/${res.data?.paused ? "resume" : "pause"}`, { body: {} }),
    onSuccess: () =>
      qc
        .invalidateQueries({ queryKey: ["status", part.id] })
        .then(() => qc.invalidateQueries({ queryKey: ["ui", "alerts"] })),
    onError: (e) => toast.error(messageOf(e)),
  });
  const d = res.data;
  const facts = d ? summarise(part.id, d) : [];
  return (
    <Card className="flex flex-col gap-2">
      <div className="flex items-center justify-between">
        <h2 className="font-semibold">{part.title}</h2>
        <Pill tone={!d ? "neutral" : d.paused ? "warning" : "success"}>
          {!d ? "..." : d.paused ? "Paused" : "Running"}
        </Pill>
      </div>
      <ul className="text-sm text-muted">
        {facts.map((f) => (
          <li key={f}>{f}</li>
        ))}
      </ul>
      {admin && d && (
        <div className="mt-1">
          <Button
            variant="secondary"
            className="h-8"
            onClick={() => (d.paused ? toggle.mutate() : setAsk(true))}
            busy={toggle.isPending}
          >
            {d.paused ? "Resume" : "Pause"}
          </Button>
        </div>
      )}
      <ConfirmDialog
        open={ask}
        onClose={() => setAsk(false)}
        danger={false}
        title={`Pause ${part.title.toLowerCase()}?`}
        text={part.keeps}
        confirmLabel="Pause"
        onConfirm={() => toggle.mutateAsync()}
      />
    </Card>
  );
}

function sum(o: unknown): number {
  return Object.values((o as Record<string, number>) ?? {}).reduce((a, b) => a + b, 0);
}
function summarise(id: string, d: Generic): string[] {
  switch (id) {
    case "sending":
      return [
        `${sum(d.jobs)} emails in the queue or done`,
        `${(d.accounts as Record<string, number>)?.active ?? 0} active accounts`,
      ];
    case "replies":
      return [
        `${sum(d.accounts)} accounts`,
        d.poller_enabled ? "Poller on" : "Poller off on this server",
      ];
    case "followups":
      return [
        `${(d.due_next_hour as number) ?? 0} due in the next hour`,
        `Last check ${fmtAgo(d.last_tick_at as string | undefined)}`,
        ...(((d.stalled as number) ?? 0) > 0 ? [`${d.stalled} stalled`] : []),
      ];
    case "campaigns":
      return [
        `${(d.active_campaigns as number) ?? 0} active campaigns`,
        `Last check ${fmtAgo(d.last_tick_at as string | undefined)}`,
        ...((d.waiting as unknown[] | undefined)?.length
          ? [`${(d.waiting as unknown[]).length} waiting for a reason`]
          : []),
      ];
    case "verifier":
      return [
        `${(d.open_runs as number) ?? 0} open runs, ${(d.pending_items as number) ?? 0} emails waiting`,
        `Last check ${fmtAgo(d.last_tick_at as string | undefined)}`,
        ...(d.integration_status === "auth_failed" ? ["Reoon refused the key"] : []),
      ];
    case "generator":
      return [
        `${sum(d.queue)} texts in the queue`,
        `Last check ${fmtAgo(d.last_tick_at as string | undefined)}`,
        ...((d.warnings as string[]) ?? []).map((w) => w.split(":")[1]?.trim() ?? w),
      ];
    default:
      return [];
  }
}
