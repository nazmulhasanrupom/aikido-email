import { useMutation, useQuery } from "@tanstack/react-query";
import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import { Button, Card, Notice, Skeleton } from "@/components/ui";
import { fmtDateTime } from "@/lib/format";
import { say } from "@/lib/words";
import { DeleteAccount } from "./w.$ws.accounts.index";

export const Route = createFileRoute("/w/$ws/accounts/$id/")({ component: Overview });

function Overview() {
  const { ws, id } = Route.useParams();
  const navigate = useNavigate();
  const accounts = useQuery(q.accounts);
  const account = accounts.data?.find((a) => a.id === id);
  const status = useQuery(q.sendStatus(id));
  const preview = useQuery({
    queryKey: ["accounts", id, "preview"],
    queryFn: () =>
      http.get<S["PreviewReply"]>("/v1/sending/accounts/{id}/preview", {
        path: { id },
        query: { count: 20 },
      }),
  });
  const reading = useQuery({
    queryKey: ["accounts", id, "reading"],
    queryFn: () =>
      http.get<S["AccountReplyStatus"]>("/v1/replies/accounts/{id}/status", { path: { id } }),
  });
  const sync = useMutation({
    mutationFn: () =>
      http.post<S["SyncNowReply"]>("/v1/replies/accounts/{id}/sync", { path: { id } }),
    onSuccess: () => {
      toast.success("Reading the inbox now.");
      void reading.refetch();
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const [deleting, setDeleting] = useState(false);
  const s = status.data;
  if (!account) return null;
  return (
    <div className="grid gap-5 lg:grid-cols-2">
      <Card className="flex flex-col gap-3">
        <h2 className="font-semibold">Status</h2>
        {account.last_error && <Notice tone="danger">{account.last_error}</Notice>}
        {s?.paused_reason && <Notice tone="warning">Paused: {s.paused_reason}</Notice>}
        {s ? (
          <dl className="grid grid-cols-2 gap-y-2 text-sm">
            <dt className="text-muted">Sent today</dt>
            <dd>
              {s.sent_today} of {s.today_limit}
            </dd>
            <dt className="text-muted">Local time</dt>
            <dd>{s.local_time}</dd>
            <dt className="text-muted">Waiting in the queue</dt>
            <dd>{s.queued_jobs}</dd>
            <dt className="text-muted">Next send</dt>
            <dd>
              {s.wait
                ? `${say("wait", s.wait.reason)} (until ${fmtDateTime(s.wait.until, s.timezone)})`
                : fmtDateTime(s.next_send_at, s.timezone)}
            </dd>
            <dt className="text-muted">Last sent</dt>
            <dd>{fmtDateTime(s.last_sent_at, s.timezone) || "Never"}</dd>
          </dl>
        ) : (
          <Skeleton rows={3} />
        )}
      </Card>
      <Card className="flex flex-col gap-3">
        <div className="flex items-center justify-between">
          <h2 className="font-semibold">Reading replies</h2>
          <Button
            variant="secondary"
            className="h-8"
            busy={sync.isPending}
            onClick={() => sync.mutate()}
            disabled={!account.can_read}
          >
            Sync now
          </Button>
        </div>
        <p className={account.can_read ? "text-success" : "text-warning"}>
          {account.can_read ? "Reading" : say("read", account.read_status)}
        </p>
        {reading.data?.folders.map((f) => (
          <div key={f.folder} className="flex justify-between text-sm">
            <span>{f.folder}</span>
            <span className="text-muted">
              {f.last_error
                ? f.last_error
                : `Last read ${fmtDateTime(f.last_success_at) || "never"}`}
            </span>
          </div>
        ))}
      </Card>
      <Card className="lg:col-span-2">
        <h2 className="mb-2 font-semibold">
          Next 20 send times{preview.data ? ` (${preview.data.timezone})` : ""}
        </h2>
        {preview.isPending && <Skeleton rows={2} />}
        {preview.data && (
          <ul className="grid grid-cols-2 gap-x-6 gap-y-0.5 text-sm md:grid-cols-4">
            {preview.data.times.map((t) => (
              <li key={t.at}>{t.local}</li>
            ))}
          </ul>
        )}
      </Card>
      <Card className="flex flex-col gap-2 border-danger/40 lg:col-span-2">
        <h2 className="font-semibold text-danger">Danger</h2>
        <p className="text-muted">
          Queued emails from this account are cancelled. Its sequences stop.
        </p>
        <div>
          <Button variant="danger" onClick={() => setDeleting(true)}>
            Delete account
          </Button>
        </div>
      </Card>
      <DeleteAccount
        open={deleting}
        onClose={() => setDeleting(false)}
        account={account}
        onDeleted={() => navigate({ to: "/w/$ws/accounts", params: { ws } })}
      />
    </div>
  );
}
