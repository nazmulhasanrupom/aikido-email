import { createFileRoute, redirect } from "@tanstack/react-router";
import { authStateQuery } from "@/api/auth";

export const Route = createFileRoute("/")({
  beforeLoad: async ({ context }) => {
    const state = await context.queryClient.ensureQueryData(authStateQuery);
    if (state.setup_needed) throw redirect({ to: "/setup" });
    if (!state.logged_in) throw redirect({ to: "/login", search: {} });
    throw redirect({ to: "/w/$ws/campaigns", params: { ws: "default" } });
  },
});
