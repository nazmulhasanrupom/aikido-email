import { createFileRoute } from "@tanstack/react-router";
import { PersonalizationPage } from "@/features/personalization/PersonalizationPage";

export const Route = createFileRoute("/w/$ws/personalization")({ component: Page });

function Page() {
  const { ws } = Route.useParams();
  return <PersonalizationPage ws={ws} />;
}
