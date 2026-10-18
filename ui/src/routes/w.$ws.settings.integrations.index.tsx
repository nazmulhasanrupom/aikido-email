import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { q } from "@/api/queries";
import { Button, ConfirmDialog, Pill } from "@/components/ui";
import { InstallDialog } from "@/features/reoon/dialogs";
import { appsQuery, useReoon } from "@/features/reoon/api";
import { useUser } from "@/features/settings/useUser";
import { http } from "@/api/http";
import { messageOf } from "@/api/errors";
import { useMutation, useQueryClient } from "@tanstack/react-query";
import { Settings as Gear, ShieldCheck } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { CATEGORIES, INTEGRATIONS } from "@/features/integrations/registry";

export const Route = createFileRoute("/w/$ws/settings/integrations/")({ component: Integrations });

/** The Reoon card: Install, or Installed with a gear and Uninstall, or Key refused. */
function ReoonCard({ ws }: { ws: string }) {
  const qc = useQueryClient();
  const reoon = useReoon();
  const { isAdmin } = useUser();
  const [installing, setInstalling] = useState(false);
  const [removing, setRemoving] = useState(false);
  const uninstall = useMutation({
    mutationFn: () => http.delEmpty("/v1/integrations/apps/reoon"),
    onSuccess: () => qc.invalidateQueries({ queryKey: appsQuery.queryKey }),
    onError: (e) => toast.error(messageOf(e)),
  });
  const broken = reoon.status === "auth_failed";
  return (
    <div className="flex gap-3 rounded-(--radius-card) border border-border p-4">
      <ShieldCheck size={20} aria-hidden="true" className="mt-0.5 shrink-0 text-muted" />
      <div className="flex-1">
        <span className="block font-medium">Reoon Email Verifier</span>
        <span className="block text-sm text-muted">Check if lead emails exist before you send.</span>
        <div className="mt-3 flex flex-wrap items-center gap-2">
          {!reoon.installed && isAdmin && <Button onClick={() => setInstalling(true)}>Install</Button>}
          {!reoon.installed && !isAdmin && <span className="text-sm text-muted">Ask an admin</span>}
          {reoon.installed && (
            <>
              <Pill tone={broken ? "danger" : "success"}>{broken ? "Key refused" : "Installed"}</Pill>
              <Link to="/w/$ws/apps/reoon" params={{ ws }} className="text-sm text-accent underline" aria-label="Reoon settings">
                <Gear size={14} className="inline" aria-hidden="true" /> Open
              </Link>
              {isAdmin && broken && (
                <Link to="/w/$ws/apps/reoon" params={{ ws }} className="text-sm text-danger underline">
                  Update key
                </Link>
              )}
              {isAdmin && (
                <Button variant="ghost" className="h-7" onClick={() => setRemoving(true)}>
                  Uninstall
                </Button>
              )}
            </>
          )}
        </div>
      </div>
      <InstallDialog open={installing} onClose={() => setInstalling(false)} />
      <ConfirmDialog
        open={removing}
        onClose={() => setRemoving(false)}
        title="Uninstall Reoon?"
        confirmLabel="Uninstall"
        text="The Reoon menu goes away and the send rule stops. Your results stay."
        onConfirm={() => uninstall.mutateAsync()}
      />
    </div>
  );
}

function Integrations() {
  const { ws } = Route.useParams();
  const providers = useQuery(q.providers);
  const status = (id: string): "connected" | "not_connected" | "error" | "soon" => {
    const it = INTEGRATIONS.find((i) => i.id === id);
    if (it?.soon) return "soon";
    if (id === "ai") {
      const list = providers.data ?? [];
      if (list.some((p) => p.status !== "ok")) return "error";
      return list.length ? "connected" : "not_connected";
    }
    return "not_connected";
  };
  return (
    <div className="flex flex-col gap-6">
      {CATEGORIES.map((cat) => (
        <section key={cat} aria-label={cat}>
          <h2 className="mb-2 text-sm font-semibold text-muted">{cat}</h2>
          <div className="grid gap-3 sm:grid-cols-2">
            {INTEGRATIONS.filter((i) => i.category === cat).map((i) => {
              if (i.id === "reoon") return <ReoonCard key={i.id} ws={ws} />;
              const st = status(i.id);
              const body = (
                <>
                  <i.icon size={20} aria-hidden="true" className="mt-0.5 shrink-0 text-muted" />
                  <span className="flex-1">
                    <span className="block font-medium">{i.name}</span>
                    <span className="block text-sm text-muted">{i.description}</span>
                  </span>
                  <Pill
                    tone={st === "connected" ? "success" : st === "error" ? "danger" : "neutral"}
                  >
                    {
                      {
                        connected: "Connected",
                        not_connected: "Not connected",
                        error: "Needs attention",
                        soon: "Soon",
                      }[st]
                    }
                  </Pill>
                </>
              );
              return i.soon ? (
                <div
                  key={i.id}
                  className="flex gap-3 rounded-(--radius-card) border border-border p-4 opacity-70"
                >
                  {body}
                </div>
              ) : (
                <Link
                  key={i.id}
                  to="/w/$ws/settings/integrations/$id"
                  params={{ ws, id: i.id }}
                  className="flex gap-3 rounded-(--radius-card) border border-border p-4 hover:bg-surface-2"
                >
                  {body}
                </Link>
              );
            })}
          </div>
        </section>
      ))}
    </div>
  );
}
