import { createFileRoute, useLocation } from "@tanstack/react-router";
import { Guide } from "@/features/guide/Guide";

export const Route = createFileRoute("/w/$ws/guide")({ component: GuidePage });

function GuidePage() {
  const { ws } = Route.useParams();
  const hash = useLocation({ select: (l) => l.hash });
  return <Guide ws={ws} hash={hash || undefined} />;
}
