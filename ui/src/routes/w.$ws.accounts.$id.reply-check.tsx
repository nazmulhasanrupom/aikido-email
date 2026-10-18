import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useRef, useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { Button, Card, ErrorNote, Notice, Skeleton } from "@/components/ui";
import { ReplyForm } from "@/features/settings/ReplyForm";
import { say } from "@/lib/words";

export const Route = createFileRoute("/w/$ws/accounts/$id/reply-check")({ component: ReplyCheck });

function ReplyCheck() {
  const { id } = Route.useParams();
  const qc = useQueryClient();
  const key = ["accounts", id, "reply-settings"];
  const res = useQuery({
    queryKey: key,
    queryFn: () =>
      http.get<{ settings: S["ReplySettings"] }>("/v1/replies/accounts/{id}/settings", {
        path: { id },
      }),
  });
  const reading = useQuery({
    queryKey: ["accounts", id, "reading"],
    queryFn: () =>
      http.get<S["AccountReplyStatus"]>("/v1/replies/accounts/{id}/status", { path: { id } }),
  });
  const folders = reading.data?.folders ?? [];
  const paused = folders.length > 0 && folders.every((f) => f.paused);
  const act = useMutation({
    mutationFn: (what: "pause" | "resume") =>
      http.post(`/v1/replies/accounts/{id}/${what}`, { path: { id }, body: {} }),
    onSuccess: (_, what) => {
      toast.success(what === "pause" ? "Reply check paused." : "Reply check running.");
      void qc.invalidateQueries({ queryKey: ["accounts", id, "reading"] });
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  if (res.isPending) return <Skeleton />;
  if (res.isError) return <ErrorNote error={res.error} />;
  return (
    <div className="flex flex-col gap-8">
      <div className="flex items-center gap-3">
        <Button
          variant={paused ? "primary" : "secondary"}
          busy={act.isPending}
          disabled={reading.isPending}
          onClick={() => act.mutate(paused ? "resume" : "pause")}
        >
          {paused ? "Start reply check" : "Stop reply check"}
        </Button>
        {reading.data && (
          <span className={`text-sm ${paused ? "text-warning" : "text-success"}`}>
            {paused ? "Stopped" : "Running"}
          </span>
        )}
      </div>
      <ReplyForm
        value={res.data.settings}
        onSave={async (settings) => {
          qc.setQueryData(
            key,
            await http.patch("/v1/replies/accounts/{id}/settings", {
              path: { id },
              body: settings,
            }),
          );
        }}
        onReset={async () => {
          qc.setQueryData(
            key,
            await http.post("/v1/replies/accounts/{id}/settings/reset", { path: { id } }),
          );
        }}
      />
      <Classifier />
    </div>
  );
}

/** Try the keyword lists on one .eml file. Nothing is saved. */
function Classifier() {
  const file = useRef<HTMLInputElement>(null);
  const [result, setResult] = useState<S["ClassifyReply"] | null>(null);
  const [error, setError] = useState<string | null>(null);
  async function run(f: File) {
    setError(null);
    try {
      const bytes = new Uint8Array(await f.arrayBuffer());
      let bin = "";
      for (const b of bytes) bin += String.fromCharCode(b);
      setResult(
        await http.post<S["ClassifyReply"]>("/v1/replies/classify", { body: { raw: btoa(bin) } }),
      );
    } catch (e) {
      setError(messageOf(e));
    }
  }
  return (
    <Card className="flex max-w-3xl flex-col gap-3">
      <h2 className="font-semibold">Test a keyword list</h2>
      <p className="text-muted">
        Choose an .eml file. We show the label it would get. Nothing is saved.
      </p>
      <input
        ref={file}
        type="file"
        accept=".eml,message/rfc822"
        aria-label="Email file (.eml)"
        onChange={(e) => e.target.files?.[0] && run(e.target.files[0])}
      />
      {error && <Notice tone="danger">{error}</Notice>}
      {result && (
        <Notice>
          <b>{say("label", result.label)}</b>
          <p className="text-xs text-muted">{result.reason}</p>
          <pre className="mt-1 whitespace-pre-wrap text-xs">{JSON.stringify(result, null, 2)}</pre>
        </Notice>
      )}
    </Card>
  );
}
