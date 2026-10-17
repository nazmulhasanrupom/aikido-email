import { useInfiniteQuery, useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import {
  Button,
  Card,
  Check2,
  ConfirmDialog,
  EmptyState,
  ErrorNote,
  Head,
  Modal,
  NumField,
  Pill,
  Select,
  Skeleton,
  Table,
  TD,
  TextArea,
} from "@/components/ui";
import { campaignQuery } from "@/features/campaigns/hooks";
import { fmtDateTime, fullName } from "@/lib/format";
import { say } from "@/lib/words";

export const Route = createFileRoute("/w/$ws/campaigns/$id/followups")({ component: Followups });

function Followups() {
  const { id } = Route.useParams();
  const camp = useQuery(campaignQuery(id));
  if (!camp.data) return <Skeleton />;
  return (
    <div className="flex flex-col gap-6">
      <Rules seq={camp.data.sequence} />
      <ReviewQueue campaignId={id} />
      <Enrollments sequenceId={camp.data.sequence.id} />
    </div>
  );
}

function Rules({ seq }: { seq: S["Sequence"] }) {
  const qc = useQueryClient();
  const [s, setS] = useState({
    thread: seq.thread_followups,
    stop: seq.stop_on_auto_reply,
    days: seq.auto_reply_pause_days,
  });
  const save = useMutation({
    mutationFn: () =>
      http.patch("/v1/followups/sequences/{id}", {
        path: { id: seq.id },
        body: {
          thread_followups: s.thread,
          stop_on_auto_reply: s.stop,
          auto_reply_pause_days: s.days,
        },
      }),
    onSuccess: () => {
      toast.success("Saved.");
      void qc.invalidateQueries({ queryKey: ["campaigns"] });
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  return (
    <Card className="flex max-w-2xl flex-col gap-3">
      <h2 className="font-semibold">Follow-up rules</h2>
      <Check2
        label="Keep follow-ups in the same thread"
        checked={s.thread}
        onChange={(thread) => setS({ ...s, thread })}
      />
      <Select
        label="On an out-of-office reply"
        value={s.stop ? "stop" : "pause"}
        onChange={(v) => setS({ ...s, stop: v === "stop" })}
        options={[
          { value: "pause", label: "Pause, then go on" },
          { value: "stop", label: "Stop the sequence" },
        ]}
      />
      {!s.stop && (
        <NumField
          label="Pause for"
          value={s.days}
          min={1}
          suffix="business days"
          onChange={(days) => setS({ ...s, days })}
        />
      )}
      <Check2
        label="A reply, bounce, unsubscribe or spam complaint always stops the sequence."
        checked
        disabled
      />
      <div>
        <Button busy={save.isPending} onClick={() => save.mutate()}>
          Save
        </Button>
      </div>
    </Card>
  );
}

function LeadName({ id }: { id: string }) {
  const l = useQuery({
    queryKey: ["leads", "one", id],
    queryFn: () => http.get<S["LeadDetail"]>("/v1/leads/{id}", { path: { id } }),
    staleTime: 60_000,
  });
  return <span>{l.data ? fullName(l.data) || l.data.email : "..."}</span>;
}

function ReviewQueue({ campaignId }: { campaignId: string }) {
  const qc = useQueryClient();
  const key = ["generator", "generations", campaignId];
  const list = useQuery({
    queryKey: key,
    queryFn: async () => {
      const [review, failed] = await Promise.all([
        http.get<{ generations: S["Generation"][] }>("/v1/generator/generations", {
          query: { campaign_id: campaignId, status: "pending_review", limit: 50 },
        }),
        http.get<{ generations: S["Generation"][] }>("/v1/generator/generations", {
          query: { campaign_id: campaignId, status: "failed", limit: 20 },
        }),
      ]);
      return { review: review.generations, failed: failed.generations };
    },
    refetchInterval: 30_000,
  });
  const [edit, setEdit] = useState<S["Generation"] | null>(null);
  const [slots, setSlots] = useState<Record<string, string>>({});
  const [all, setAll] = useState(false);
  const refresh = () => qc.invalidateQueries({ queryKey: key });
  const act = useMutation({
    mutationFn: ({ g, what }: { g: S["Generation"]; what: "approve" | "reject" }) =>
      http.post(`/v1/generator/generations/{id}/${what}`, { path: { id: g.id } }),
    onSuccess: refresh,
    onError: (e) => toast.error(messageOf(e)),
  });
  const saveEdit = useMutation({
    mutationFn: () =>
      http.patch("/v1/generator/generations/{id}", {
        path: { id: (edit as S["Generation"]).id },
        body: { slots },
      }),
    onSuccess: () => {
      setEdit(null);
      void refresh();
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  if (!list.data || (list.data.review.length === 0 && list.data.failed.length === 0)) return null;
  const card = (g: S["Generation"], failed: boolean) => (
    <Card key={g.id} className="flex flex-col gap-2 p-4">
      <div className="flex items-center justify-between">
        <span className="font-medium">
          <LeadName id={g.lead_id} /> <span className="text-muted">step {g.step_number}</span>
        </span>
        <Pill tone={failed ? "danger" : "ai"}>{failed ? "Failed" : "AI text"}</Pill>
      </div>
      {Object.entries(g.slots).map(([k, v]) => (
        <p key={k} className="rounded bg-ai/10 px-2 py-1 text-sm">
          <span className="text-xs text-ai">{k}:</span> {String(v)}
        </p>
      ))}
      {g.warnings.length > 0 && <p className="text-xs text-warning">{g.warnings.join(" ")}</p>}
      <div className="flex gap-2">
        {!failed && (
          <Button className="h-8" onClick={() => act.mutate({ g, what: "approve" })}>
            Approve
          </Button>
        )}
        <Button
          variant="secondary"
          className="h-8"
          onClick={() => {
            setEdit(g);
            setSlots(Object.fromEntries(Object.entries(g.slots).map(([k, v]) => [k, String(v)])));
          }}
        >
          {failed ? "Edit by hand" : "Edit"}
        </Button>
        {!failed && (
          <Button variant="ghost" className="h-8" onClick={() => act.mutate({ g, what: "reject" })}>
            Write again
          </Button>
        )}
      </div>
    </Card>
  );
  return (
    <section className="flex flex-col gap-3" aria-label="Waiting for you">
      <div className="flex items-center justify-between">
        <h2 className="font-semibold">Waiting for you ({list.data.review.length})</h2>
        {list.data.review.length > 1 && (
          <Button variant="secondary" onClick={() => setAll(true)}>
            Approve all
          </Button>
        )}
      </div>
      <div className="grid gap-3 lg:grid-cols-2">
        {list.data.review.map((g) => card(g, false))}
        {list.data.failed.map((g) => card(g, true))}
      </div>
      <Modal
        open={!!edit}
        onClose={() => setEdit(null)}
        title="Edit the AI text"
        footer={
          <>
            <Button variant="secondary" onClick={() => setEdit(null)}>
              Cancel
            </Button>
            <Button busy={saveEdit.isPending} onClick={() => saveEdit.mutate()}>
              Save and approve
            </Button>
          </>
        }
      >
        <div className="flex flex-col gap-3">
          {edit &&
            Object.keys(edit.slots)
              .concat(Object.keys(slots).filter((k) => !(k in edit.slots)))
              .map((k) => (
                <TextArea
                  key={k}
                  label={k}
                  value={slots[k] ?? ""}
                  onChange={(e) => setSlots({ ...slots, [k]: e.target.value })}
                />
              ))}
        </div>
      </Modal>
      <ConfirmDialog
        open={all}
        onClose={() => setAll(false)}
        title="Approve all texts?"
        danger={false}
        confirmLabel="Approve all"
        text={`${list.data.review.length} AI texts will go out as they are.`}
        onConfirm={async () => {
          await http.post("/v1/generator/generations/bulk", {
            body: {
              action: "approve",
              filter: { campaign_id: campaignId, status: "pending_review" },
            },
          });
          await refresh();
        }}
      />
    </section>
  );
}

function Enrollments({ sequenceId }: { sequenceId: string }) {
  const qc = useQueryClient();
  const [status, setStatus] = useState("");
  const [open, setOpen] = useState<string | null>(null);
  const list = useInfiniteQuery({
    queryKey: ["followups", "enrollments", sequenceId, status],
    initialPageParam: undefined as string | undefined,
    queryFn: ({ pageParam }) =>
      http.get<S["ListEnrollmentsReply"]>("/v1/followups/enrollments", {
        query: { sequence_id: sequenceId, status, limit: 50, cursor: pageParam },
      }),
    getNextPageParam: (l) => l.next_cursor ?? undefined,
    refetchInterval: 15_000,
  });
  const act = useMutation({
    mutationFn: ({ id, what }: { id: string; what: string }) =>
      http.post(`/v1/followups/enrollments/{id}/${what}`, { path: { id }, body: {} }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["followups", "enrollments"] }),
    onError: (e) => toast.error(messageOf(e)),
  });
  const rows = list.data?.pages.flatMap((p) => p.enrollments) ?? [];
  return (
    <section className="flex flex-col gap-3" aria-label="Enrollments">
      <div className="flex items-end justify-between">
        <h2 className="font-semibold">Who is where</h2>
        <div className="w-44">
          <Select
            label="Status"
            value={status}
            onChange={setStatus}
            options={[
              { value: "", label: "All" },
              ...["active", "waiting_send", "paused", "finished", "stopped", "failed"].map((s) => ({
                value: s,
                label: say("enrollment", s),
              })),
            ]}
          />
        </div>
      </div>
      {list.isPending && <Skeleton rows={3} />}
      {list.isError && <ErrorNote error={list.error} />}
      {list.data && rows.length === 0 && <EmptyState text="Nobody is in this sequence yet." />}
      {rows.length > 0 && (
        <Table>
          <Head cols={["Lead", "Step", "Status", "Next send", "Reason", ""]} />
          <tbody>
            {rows.map((e) => (
              <tr key={e.id} className="border-t border-border">
                <td className={TD}>
                  <button
                    type="button"
                    className="font-medium hover:underline"
                    onClick={() => setOpen(e.id)}
                  >
                    {e.to_email}
                  </button>
                </td>
                <td className={TD}>{e.next_step}</td>
                <td className={TD}>
                  <Pill>{say("enrollment", e.status)}</Pill>
                </td>
                <td className={TD}>{fmtDateTime(e.next_due_at)}</td>
                <td className={TD}>
                  {e.stop_reason
                    ? say("stop", e.stop_reason)
                    : e.wait_reason
                      ? say("wait", e.wait_reason)
                      : ""}
                </td>
                <td className={TD}>
                  {(e.status === "active" || e.status === "waiting_send") && (
                    <Button
                      variant="ghost"
                      className="h-7"
                      onClick={() => act.mutate({ id: e.id, what: "pause" })}
                    >
                      Pause
                    </Button>
                  )}
                  {e.status === "paused" && (
                    <Button
                      variant="ghost"
                      className="h-7"
                      onClick={() => act.mutate({ id: e.id, what: "resume" })}
                    >
                      Resume
                    </Button>
                  )}
                  {["active", "waiting_send", "paused"].includes(e.status) && (
                    <Button
                      variant="ghost"
                      className="h-7"
                      onClick={() => act.mutate({ id: e.id, what: "stop" })}
                    >
                      Stop
                    </Button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </Table>
      )}
      {list.hasNextPage && (
        <div>
          <Button variant="secondary" onClick={() => list.fetchNextPage()}>
            Load more
          </Button>
        </div>
      )}
      <EnrollmentDrawer id={open} onClose={() => setOpen(null)} />
    </section>
  );
}

function EnrollmentDrawer({ id, onClose }: { id: string | null; onClose: () => void }) {
  const d = useQuery({
    queryKey: ["followups", "enrollment", id],
    enabled: !!id,
    queryFn: () =>
      http.get<S["EnrollmentDetail"]>("/v1/followups/enrollments/{id}", {
        path: { id: id as string },
      }),
  });
  return (
    <Modal open={!!id} onClose={onClose} side title={d.data?.enrollment.to_email ?? "Enrollment"}>
      {d.isPending && <Skeleton rows={4} />}
      {d.data && (
        <div className="flex flex-col gap-5 text-sm">
          <section>
            <h3 className="mb-2 font-semibold">Events</h3>
            <ul className="flex flex-col gap-1.5">
              {d.data.events.map((e) => (
                <li key={e.id} className="flex justify-between gap-3">
                  <span>
                    {e.kind.replaceAll("_", " ")}
                    {e.step_number ? ` (step ${e.step_number})` : ""}
                  </span>
                  <span className="text-muted">{fmtDateTime(e.at)}</span>
                </li>
              ))}
            </ul>
          </section>
          <section>
            <h3 className="mb-2 font-semibold">Emails</h3>
            <ul className="flex flex-col gap-1.5">
              {d.data.jobs.map((j) => (
                <li key={j.id} className="flex justify-between gap-3">
                  <span className="truncate">{j.subject}</span>
                  <Pill>{say("job", j.status)}</Pill>
                </li>
              ))}
              {d.data.jobs.length === 0 && <li className="text-muted">None yet.</li>}
            </ul>
          </section>
        </div>
      )}
    </Modal>
  );
}
