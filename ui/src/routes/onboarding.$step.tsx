import { useQueryClient } from "@tanstack/react-query";
import { createFileRoute, redirect, useNavigate } from "@tanstack/react-router";
import { useState } from "react";
import { authStateQuery } from "@/api/auth";
import { messageOf } from "@/api/errors";
import { http } from "@/api/http";
import { q } from "@/api/queries";
import { Logo } from "@/components/Logo";
import { Button, Card, Notice } from "@/components/ui";
import { DesignPicker } from "@/features/settings/DesignPicker";

export const Route = createFileRoute("/onboarding/$step")({
  beforeLoad: async ({ context }) => {
    const state = await context.queryClient.ensureQueryData(authStateQuery);
    if (!state.logged_in) throw redirect({ to: "/login", search: {} });
  },
  component: Onboarding,
});

/** The only step: pick a design. Every `$step` value shows it (old saved links keep working). */
function Onboarding() {
  const navigate = useNavigate();
  const qc = useQueryClient();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function finish() {
    setBusy(true);
    setError(null);
    try {
      await http.patch("/v1/ui/onboarding", { body: { done: true, step: "done" } });
      await qc.invalidateQueries({ queryKey: q.onboarding.queryKey });
      await navigate({ to: "/w/$ws", params: { ws: "default" } });
    } catch (e) {
      setError(messageOf(e));
    } finally {
      setBusy(false);
    }
  }

  return (
    <main className="mx-auto flex min-h-full max-w-3xl flex-col gap-6 px-4 py-10">
      <div className="flex items-center justify-center gap-3">
        <Logo size={32} />
        <span className="text-lg font-semibold tracking-tight">AIKIDO EMAIL</span>
      </div>
      <Card className="flex flex-col gap-5">
        <div>
          <h1 className="text-xl font-semibold">Pick a design</h1>
          <p className="text-muted">
            Click a card to try it. You can change it later in your account menu.
          </p>
        </div>
        <DesignPicker />
        {error && <Notice tone="danger">{error}</Notice>}
        <div className="flex justify-end border-t border-border pt-4">
          <Button busy={busy} onClick={() => void finish()}>
            Go to dashboard
          </Button>
        </div>
      </Card>
    </main>
  );
}
