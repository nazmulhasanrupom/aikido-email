import { createFileRoute } from "@tanstack/react-router";
import { Notice } from "@/components/ui";
import { EnvironmentForm } from "@/features/settings/EnvironmentForm";
import { useUser } from "@/features/settings/useUser";

export const Route = createFileRoute("/w/$ws/settings/environment")({ component: Environment });

function Environment() {
  const { ws } = Route.useParams();
  const { isAdmin } = useUser();
  if (!isAdmin) return <Notice tone="warning">Only an owner or an admin can change these.</Notice>;
  return <EnvironmentForm ws={ws} />;
}
