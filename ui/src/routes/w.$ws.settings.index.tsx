import { createFileRoute, redirect } from "@tanstack/react-router";

export const Route = createFileRoute("/w/$ws/settings/")({
  beforeLoad: ({ params }) => {
    throw redirect({ to: "/w/$ws/settings/account", params });
  },
});
