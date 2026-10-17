import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import { useUser } from "@/features/settings/useUser";
import { Button } from "../Button";

/** One thin bar under the top bar, the most serious thing first. */
export function Banners({ ws }: { ws: string }) {
  const { isAdmin } = useUser();
  const qc = useQueryClient();
  const engine = useQuery({
    queryKey: ["status", "sending"],
    queryFn: () => http.get<S["EngineStatus"]>("/v1/sending/engine/status"),
    refetchInterval: 30_000,
  });
  const cfg = useQuery(q.config);
  const alerts = useQuery(q.alerts);
  const resume = useMutation({
    mutationFn: () => http.post("/v1/sending/engine/resume", { body: {} }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["status"] }),
  });
  const down = (alerts.data ?? []).filter((a) => a.kind === "account_disconnected").length;
  const base = "flex items-center justify-center gap-3 px-4 py-1.5 text-sm";
  if (engine.data?.paused)
    return (
      <div role="status" className={`${base} bg-warning/15`}>
        Sending is paused for everyone.
        {isAdmin && (
          <Button
            variant="secondary"
            className="h-7"
            busy={resume.isPending}
            onClick={() => resume.mutate()}
          >
            Resume
          </Button>
        )}
      </div>
    );
  if (cfg.data?.dry_run)
    return (
      <div role="status" className={`${base} bg-info/15`}>
        Dry run: emails are logged, not sent.
      </div>
    );
  if (down > 0)
    return (
      <div role="status" className={`${base} bg-danger/10`}>
        {down} account{down === 1 ? " is" : "s are"} disconnected.
        <Link to="/w/$ws/accounts" params={{ ws }} className="underline">
          Fix
        </Link>
      </div>
    );
  return null;
}
