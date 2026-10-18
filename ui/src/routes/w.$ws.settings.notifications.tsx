import { useMutation, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { toast } from "sonner";
import { authStateKey } from "@/api/auth";
import { messageOf } from "@/api/errors";
import { http } from "@/api/http";
import { Card, Check2, Notice } from "@/components/ui";
import { useUser } from "@/features/settings/useUser";

export const Route = createFileRoute("/w/$ws/settings/notifications")({ component: Notifications });

export const ALERT_KINDS: [string, string][] = [
  ["account_disconnected", "An email account lost its connection"],
  ["account_error", "An email account stopped because of a problem"],
  ["account_reconnect_needed", "We cannot see the replies of an account"],
  ["campaign_warning", "A campaign has a warning"],
  ["campaign_no_healthy_account", "A campaign has no account that can send"],
  ["generation_awaiting_review", "AI texts wait for your review"],
  ["provider_auth_failed", "An AI model refused our key"],
  ["ai_budget_reached", "The AI token limit is reached"],
  ["emails_not_ready", "A due email is not written or not approved"],
  ["import_failed", "An import failed"],
  ["jobs_stuck", "Emails have an unknown result"],
  ["engine_paused", "A part of the system is paused"],
];

function Notifications() {
  const { user } = useUser();
  const qc = useQueryClient();
  const prefs =
    (user?.preferences as { notifications?: Record<string, boolean> } | undefined)?.notifications ??
    {};
  const save = useMutation({
    mutationFn: (n: Record<string, boolean>) =>
      http.patch("/v1/users/me", { body: { preferences: { notifications: n } } }),
    onSuccess: () =>
      qc
        .invalidateQueries({ queryKey: authStateKey })
        .then(() => qc.invalidateQueries({ queryKey: ["ui", "alerts"] })),
    onError: (e) => toast.error(messageOf(e)),
  });
  return (
    <div className="flex max-w-2xl flex-col gap-4">
      <Card className="flex flex-col gap-3">
        <h2 className="font-semibold">Show me an alert when</h2>
        {ALERT_KINDS.map(([k, label]) => (
          <Check2
            key={k}
            label={label}
            checked={prefs[k] !== false}
            onChange={(on) => save.mutate({ ...prefs, [k]: on })}
          />
        ))}
      </Card>
      <Notice>Email alerts come later. For now alerts show in the bell at the top.</Notice>
    </div>
  );
}
