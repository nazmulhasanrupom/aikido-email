import { useQuery } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { q } from "@/api/queries";
import { Pill } from "@/components/ui";
import { CATEGORIES, INTEGRATIONS } from "@/features/integrations/registry";

export const Route = createFileRoute("/w/$ws/settings/integrations/")({ component: Integrations });

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
