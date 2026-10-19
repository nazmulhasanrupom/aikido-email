import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { Check, Circle } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { authStateQuery } from "@/api/auth";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { Button } from "@/components/ui";

const FIX: Record<string, "/w/$ws/campaigns/$id/settings" | "/w/$ws/campaigns/$id/leads" | "/w/$ws/campaigns/$id/editor"> = {
  settings: "/w/$ws/campaigns/$id/settings",
  leads: "/w/$ws/campaigns/$id/leads",
  editor: "/w/$ws/campaigns/$id/editor",
};

/** "4 of 6 ready" in the campaign header. It blocks nothing. Each item links to its fix. */
export function ChecklistBar({ campaignId, ws }: { campaignId: string; ws: string }) {
  const qc = useQueryClient();
  const [open, setOpen] = useState(false);
  const list = useQuery({
    queryKey: ["campaigns", campaignId, "checklist"],
    queryFn: () => http.get<S["Checklist"]>("/v1/campaigns/{id}/checklist", { path: { id: campaignId } }),
    staleTime: 15_000,
  });
  const me = useQuery(authStateQuery).data?.user?.email;
  const test = useMutation({
    mutationFn: () =>
      http.post<S["SendTestReply"]>("/v1/campaigns/{id}/emails/send-test", { path: { id: campaignId }, body: { to: me ?? "" } }),
    onSuccess: () => {
      toast.success(`A test email is on its way to ${me}.`);
      void qc.invalidateQueries({ queryKey: ["campaigns", campaignId, "checklist"] });
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  if (!list.data) return null;
  const d = list.data;
  const done = d.ready === d.total;
  return (
    <div className="mb-3 rounded-(--radius-card) border border-border">
      <button
        type="button"
        aria-expanded={open}
        onClick={() => setOpen(!open)}
        className="flex w-full items-center justify-between px-3 py-2 text-sm"
      >
        <span className="font-medium">
          {d.ready} of {d.total} ready
        </span>
        <span className="text-muted">{done ? "Everything is ready." : open ? "Hide" : "Show what is left"}</span>
      </button>
      {open && (
        <ul className="border-t border-border px-3 py-2 text-sm">
          {d.items.map((i) => (
            <li key={i.key} className="flex items-start gap-2 py-1">
              {i.ok ? <Check size={15} className="mt-0.5 text-success" /> : <Circle size={15} className="mt-0.5 text-warning" />}
              <div className="flex-1">
                <span className="font-medium">{i.label}</span>
                <span className="block text-xs text-muted">{i.detail}</span>
              </div>
              {!i.ok && i.key === "test" && (
                <Button variant="secondary" className="h-7" busy={test.isPending} disabled={!me} onClick={() => test.mutate()}>
                  Send to me
                </Button>
              )}
              {!i.ok && i.key !== "test" && (
                <Link to={FIX[i.fix] ?? FIX.editor ?? "/w/$ws/campaigns/$id/editor"} params={{ ws, id: campaignId }} className="text-accent underline">
                  Fix
                </Link>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
