import { createFileRoute } from "@tanstack/react-router";
import { SecretsPage } from "@/features/automations/ConfigPages";

export const Route = createFileRoute("/w/$ws/automations/secrets")({ component: Page });

function Page() {
  const { ws } = Route.useParams();
  return <SecretsPage ws={ws} />;
}
