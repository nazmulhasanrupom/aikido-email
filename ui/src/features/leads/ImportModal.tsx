import { Modal } from "@/components/ui";
import { ImportFlow } from "./ImportFlow";

export function ImportModal({
  open,
  onClose,
  campaignId,
}: {
  open: boolean;
  onClose: () => void;
  campaignId?: string;
}) {
  return (
    <Modal open={open} onClose={onClose} size="lg" title="Import leads from a CSV file">
      {open && <ImportFlow campaignId={campaignId} />}
    </Modal>
  );
}
