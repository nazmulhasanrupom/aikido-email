import { createFileRoute } from "@tanstack/react-router";
import { SettingsPage } from "@/features/automations/ConfigPages";

export const Route = createFileRoute("/w/$ws/automations/settings")({ component: Page });

function Page() {
  const { ws } = Route.useParams();
  return <SettingsPage ws={ws} />;
}
