import { useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import {
  Button,
  EmptyState,
  ErrorNote,
  Head,
  Modal,
  Pill,
  Skeleton,
  Table,
  TD,
} from "@/components/ui";
import { fmtDateTime } from "@/lib/format";
import { say } from "@/lib/words";

export const Route = createFileRoute("/w/$ws/accounts/$id/activity")({ component: Activity });

function Activity() {
  const { id } = Route.useParams();
  const qc = useQueryClient();
  const jobs = useQuery({
    queryKey: ["accounts", id, "jobs"],
    queryFn: () =>
      http.get<S["ListJobsReply"]>("/v1/sending/jobs", { query: { account_id: id, limit: 50 } }),
  });
  const [open, setOpen] = useState<string | null>(null);
  if (jobs.isPending) return <Skeleton />;
  if (jobs.isError) return <ErrorNote error={jobs.error} />;
  if (jobs.data.jobs.length === 0)
    return <EmptyState text="This account has not sent anything yet." />;
  return (
    <>
      <Table>
        <Head cols={["To", "Subject", "Status", "Time", ""]} />
        <tbody>
          {jobs.data.jobs.map((j) => (
            <tr key={j.id} className="border-t border-border">
              <td className={TD}>{j.to_email}</td>
              <td className={`${TD} max-w-xs truncate`}>{j.subject}</td>
              <td className={TD}>
                <Pill
                  tone={
                    j.status === "sent"
                      ? "success"
                      : j.status === "failed" || j.status === "stuck"
                        ? "danger"
                        : "neutral"
                  }
                >
                  {say("job", j.status)}
                </Pill>
              </td>
              <td className={TD}>{fmtDateTime(j.sent_at ?? j.created_at)}</td>
              <td className={TD}>
                <Button variant="ghost" className="h-7" onClick={() => setOpen(j.id)}>
                  Details
                </Button>
                {(j.status === "failed" || j.status === "stuck") && (
                  <Button
                    variant="ghost"
                    className="h-7"
                    onClick={async () => {
                      if (
                        j.status === "stuck" &&
                        !window.confirm("It may already have been delivered. Send again?")
                      )
                        return;
                      try {
                        await http.post("/v1/sending/jobs/{id}/requeue", { path: { id: j.id } });
                        toast.success("Queued again.");
                        void qc.invalidateQueries({ queryKey: ["accounts", id, "jobs"] });
                      } catch (e) {
                        toast.error(messageOf(e));
                      }
                    }}
                  >
                    Send again
                  </Button>
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </Table>
      <JobEvents id={open} onClose={() => setOpen(null)} />
    </>
  );
}

function JobEvents({ id, onClose }: { id: string | null; onClose: () => void }) {
  const detail = useQuery({
    queryKey: ["job", id],
    enabled: !!id,
    queryFn: () =>
      http.get<S["JobDetail"]>("/v1/sending/jobs/{id}", { path: { id: id as string } }),
  });
  return (
    <Modal open={!!id} onClose={onClose} title="What happened to this email">
      {detail.isPending && <Skeleton rows={3} />}
      {detail.data && (
        <ul className="flex flex-col gap-2 text-sm">
          {detail.data.job.last_error && (
            <li className="text-danger">{detail.data.job.last_error}</li>
          )}
          {detail.data.events.map((e) => (
            <li key={e.id} className="flex justify-between gap-4">
              <span>{e.event.replaceAll("_", " ")}</span>
              <span className="text-muted">{fmtDateTime(e.at)}</span>
            </li>
          ))}
        </ul>
      )}
    </Modal>
  );
}
