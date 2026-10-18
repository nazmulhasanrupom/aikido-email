import { useQueryClient } from "@tanstack/react-query";
import { useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { ConfirmDialog } from "@/components/ui";
import { cn } from "@/lib/cn";

const MODES = [
  ["template", "Template"],
  ["ai", "AI"],
] as const;
type Mode = (typeof MODES)[number][0];

const HINT: Record<Mode, string> = {
  template: "You write the email once. Save fills it for every lead.",
  ai: "The AI writes each email from your prompts. Generate saves them per lead.",
};

/** Template | AI. */
export function ModeSwitch({ campaign }: { campaign: S["CampaignDetail"] }) {
  const qc = useQueryClient();
  const [ask, setAsk] = useState<Mode | null>(null);
  const live = campaign.status === "active" || campaign.status === "paused";
  const current = campaign.content_mode as Mode;
  async function change(m: Mode) {
    try {
      await http.patch("/v1/campaigns/{id}", {
        path: { id: campaign.id },
        body: { content_mode: m },
      });
      await qc.invalidateQueries({ queryKey: ["campaigns", campaign.id] });
    } catch (e) {
      toast.error(messageOf(e));
    }
  }
  return (
    <div className="flex items-center gap-3">
      <div
        role="group"
        aria-label="How the emails are written"
        className="flex rounded-(--radius-control) border border-border p-0.5 text-sm"
      >
        {MODES.map(([m, label]) => (
          <button
            key={m}
            type="button"
            aria-pressed={current === m}
            onClick={() => {
              if (current === m) return;
              if (live) setAsk(m);
              else void change(m);
            }}
            className={cn("rounded px-3 py-1", current === m && "bg-accent-soft font-medium")}
          >
            {label}
          </button>
        ))}
      </div>
      <p className="text-xs text-muted">{HINT[current] ?? ""}</p>
      <ConfirmDialog
        open={ask !== null}
        onClose={() => setAsk(null)}
        title="Change how the emails are written?"
        text="This campaign is live. Only emails that are not sent yet change. Sent emails stay as they are."
        danger={false}
        confirmLabel="Change"
        onConfirm={() => ask && change(ask)}
      />
    </div>
  );
}
