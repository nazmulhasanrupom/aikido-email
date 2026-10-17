import { createFileRoute, Link, redirect } from "@tanstack/react-router";
import { authStateQuery } from "@/api/auth";
import { AuthLayout } from "@/components/AuthLayout";

export const Route = createFileRoute("/onboarding/$step")({
  beforeLoad: async ({ context }) => {
    const state = await context.queryClient.ensureQueryData(authStateQuery);
    if (!state.logged_in) throw redirect({ to: "/login", search: {} });
  },
  component: Onboarding,
});

/** The setup steps (connect email, sending basics, AI, leads) come in the next phase. */
function Onboarding() {
  return (
    <AuthLayout title="Welcome" intro="Your account is ready.">
      <p className="text-muted">
        The guided setup (connect an email account, sending basics, AI, leads) comes next.
      </p>
      <Link
        to="/w/$ws/campaigns"
        params={{ ws: "default" }}
        className="mt-5 inline-flex h-9 items-center rounded-(--radius-control) bg-accent px-3.5 text-sm font-medium text-accent-fg"
      >
        Go to the app
      </Link>
    </AuthLayout>
  );
}
