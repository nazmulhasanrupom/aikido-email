import { createFileRoute } from "@tanstack/react-router";
import { ListPage } from "@/features/automations/ListPage";

export const Route = createFileRoute("/w/$ws/automations/")({ component: Page });

function Page() {
  const { ws } = Route.useParams();
  return <ListPage ws={ws} />;
}
