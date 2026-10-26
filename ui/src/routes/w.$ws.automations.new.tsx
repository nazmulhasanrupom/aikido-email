import { createFileRoute } from "@tanstack/react-router";
import { NewPage } from "@/features/automations/NewPage";

export const Route = createFileRoute("/w/$ws/automations/new")({ component: Page });

function Page() {
  const { ws } = Route.useParams();
  return <NewPage ws={ws} />;
}
