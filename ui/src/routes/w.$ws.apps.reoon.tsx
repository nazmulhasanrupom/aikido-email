import { createFileRoute } from "@tanstack/react-router";
import { ReoonPage, type ReoonSearch } from "@/features/reoon/ReoonPage";

const KEYS = ["q", "list_id", "tag", "campaign_id", "status", "checked", "lead"] as const;

export const Route = createFileRoute("/w/$ws/apps/reoon")({
  validateSearch: (s: Record<string, unknown>): ReoonSearch =>
    Object.fromEntries(
      KEYS.map((k) => [k, typeof s[k] === "string" && s[k] ? s[k] : undefined]).filter(([, v]) => v),
    ),
  component: Page,
});

function Page() {
  const { ws } = Route.useParams();
  return <ReoonPage ws={ws} search={Route.useSearch()} />;
}
