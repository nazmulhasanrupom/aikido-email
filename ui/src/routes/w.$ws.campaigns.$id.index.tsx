import { createFileRoute, redirect } from "@tanstack/react-router";

export const Route = createFileRoute("/w/$ws/campaigns/$id/")({
  beforeLoad: ({ params }) => {
    throw redirect({ to: "/w/$ws/campaigns/$id/reports", params });
  },
});
