import { useQuery } from "@tanstack/react-query";
import { Modal, Notice } from "@/components/ui";
import { reoonSettingsQuery, useReoon } from "@/features/reoon/api";
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
  const reoon = useReoon();
  const settings = useQuery({ ...reoonSettingsQuery, enabled: open && reoon.installed });
  return (
    <Modal open={open} onClose={onClose} size="lg" title="Import leads from a CSV file">
      {open && settings.data?.settings.auto_verify && (
        <div className="mb-3">
          <Notice tone="info">Leads from this file that have no Reoon result yet are checked after the import. You can turn this off on the Reoon page.</Notice>
        </div>
      )}
      {open && <ImportFlow campaignId={campaignId} />}
    </Modal>
  );
}
