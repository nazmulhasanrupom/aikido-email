import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { Button, Head, Table, TD } from "@/components/ui";
import { fmtPct } from "@/lib/format";
import { campaignQuery } from "./hooks";

const ENOUGH = 100;

/** Reply rate per variant of the first email. "Turn off the weaker one" shows when each has 100 sends. No auto-winner. */
export function VariantResults({ campaignId, stats }: { campaignId: string; stats: S["StatsReply"] }) {
  const qc = useQueryClient();
  const camp = useQuery(campaignQuery(campaignId));
  const rows = stats.by_variant;
  const enough = rows.length > 1 && rows.every((r) => r.sent >= ENOUGH);
  const weaker = enough ? [...rows].sort((a, b) => a.reply_rate - b.reply_rate)[0] : undefined;
  const off = useMutation({
    mutationFn: async (key: string) => {
      const c = camp.data;
      if (!c) throw new Error("The campaign is not loaded.");
      const steps = c.steps.map((s) => ({
        delay_amount: s.delay_amount,
        delay_type: s.delay_type,
        subject: s.subject,
        body_text: s.body_text,
        body_html: s.body_html,
        ai_slots: s.ai_slots,
        ai_user_prompt: s.ai_user_prompt,
      }));
      const variants = c.variants.map((v) => ({
        name: v.name,
        subject: v.subject,
        body_text: v.body_text,
        body_html: v.body_html,
        ai_slots: v.ai_slots,
        active: v.key === key ? false : v.active,
      }));
      await http.put("/v1/followups/sequences/{id}/steps", {
        path: { id: c.sequence_id },
        body: key === "A" ? { steps, variants, variant_a_active: false } : { steps, variants },
      });
    },
    onSuccess: () => {
      toast.success("Turned off. It gets no new leads. Sent emails do not change.");
      void qc.invalidateQueries({ queryKey: ["campaigns", campaignId] });
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  if (rows.length < 2) return null;
  const isOn = (key: string) => (key === "A" ? (camp.data?.sequence.variant_a_active ?? true) : (camp.data?.variants.find((v) => v.key === key)?.active ?? true));
  return (
    <section>
      <h2 className="mb-2 font-semibold">First email by variant</h2>
      <Table>
        <Head cols={["Variant", "Sent", "Replies", "Reply rate"]} />
        <tbody>
          {rows.map((r) => (
            <tr key={r.variant_key} className="border-t border-border">
              <td className={TD}>
                {r.variant_key}
                {isOn(r.variant_key) ? "" : " (off)"}
              </td>
              <td className={TD}>{r.sent}</td>
              <td className={TD}>{r.replied}</td>
              <td className={TD}>{fmtPct(r.reply_rate)}</td>
            </tr>
          ))}
        </tbody>
      </Table>
      <p className="mt-2 text-sm text-muted">
        {enough
          ? `Each variant has ${ENOUGH} sends or more. A small gap can still be luck.`
          : `Wait until each variant has ${ENOUGH} sends before you pick one.`}
      </p>
      {weaker && isOn(weaker.variant_key) && (
        <Button className="mt-2" variant="secondary" busy={off.isPending} onClick={() => off.mutate(weaker.variant_key)}>
          Turn off the weaker one ({weaker.variant_key})
        </Button>
      )}
    </section>
  );
}
