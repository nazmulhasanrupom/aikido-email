import { useMutation } from "@tanstack/react-query";
import { useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { Field } from "@/components/Field";
import { Button, Modal } from "@/components/ui";

const EMPTY = { email: "", first_name: "", last_name: "", company_name: "" };

/** Makes one lead from a short form and puts it in the campaign. */
export function ManualLeadModal({
  open,
  onClose,
  campaignId,
}: {
  open: boolean;
  onClose: () => void;
  campaignId: string;
}) {
  const [v, setV] = useState(EMPTY);
  const close = () => {
    setV(EMPTY);
    onClose();
  };
  const add = useMutation({
    mutationFn: async () => {
      const text = (s: string) => s.trim() || null;
      const lead = await http.post<S["LeadDetail"]>("/v1/leads", {
        body: {
          email: v.email.trim(),
          first_name: text(v.first_name),
          last_name: text(v.last_name),
          company_name: text(v.company_name),
        },
      });
      return http.post<S["AddLeadsReply"]>("/v1/campaigns/{id}/leads", {
        path: { id: campaignId },
        body: { lead_ids: [lead.id] },
      });
    },
    onSuccess: () => {
      toast.success("Lead added.");
      close();
    },
    onError: (e) => toast.error(messageOf(e)),
  });
  const set = (k: keyof typeof EMPTY) => (e: React.ChangeEvent<HTMLInputElement>) =>
    setV({ ...v, [k]: e.target.value });
  return (
    <Modal
      open={open}
      onClose={close}
      size="md"
      title="Add a lead"
      footer={
        <>
          <Button variant="secondary" onClick={close}>
            Cancel
          </Button>
          <Button busy={add.isPending} disabled={!v.email.trim()} onClick={() => add.mutate()}>
            Add lead
          </Button>
        </>
      }
    >
      <form
        className="flex flex-col gap-3"
        onSubmit={(e) => {
          e.preventDefault();
          if (v.email.trim()) add.mutate();
        }}
      >
        <Field label="Email" type="email" value={v.email} onChange={set("email")} autoFocus />
        <Field label="First name" value={v.first_name} onChange={set("first_name")} />
        <Field label="Last name" value={v.last_name} onChange={set("last_name")} />
        <Field label="Company" value={v.company_name} onChange={set("company_name")} />
      </form>
    </Modal>
  );
}
