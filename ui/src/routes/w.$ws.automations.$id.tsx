import { createFileRoute, useLocation } from "@tanstack/react-router";
import { EditorPage } from "@/features/automations/EditorPage";

// The editor keeps its state across its tabs, so this layout draws it for all three child routes.
export const Route = createFileRoute("/w/$ws/automations/$id")({ component: Page });

function Page() {
  const { ws, id } = Route.useParams();
  const path = useLocation({ select: (l) => l.pathname });
  const rest = path.split(`/automations/${id}`)[1] ?? "";
  const m = /^\/executions(?:\/([^/]+))?/.exec(rest);
  return <EditorPage ws={ws} id={id} tab={m ? "executions" : "editor"} runId={m?.[1]} />;
}
