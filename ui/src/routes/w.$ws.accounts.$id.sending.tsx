import { useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { http, type S } from "@/api/http";
import { ErrorNote, Skeleton } from "@/components/ui";
import { SendingForm } from "@/features/settings/SendingForm";

export const Route = createFileRoute("/w/$ws/accounts/$id/sending")({ component: Sending });

function Sending() {
  const { id } = Route.useParams();
  const qc = useQueryClient();
  const key = ["accounts", id, "sending-settings"];
  const res = useQuery({
    queryKey: key,
    queryFn: () =>
      http.get<S["SettingsReply"]>("/v1/sending/accounts/{id}/settings", { path: { id } }),
  });
  if (res.isPending) return <Skeleton />;
  if (res.isError) return <ErrorNote error={res.error} />;
  return (
    <SendingForm
      value={res.data.settings}
      warnings={res.data.warnings}
      onSave={async (settings) => {
        const r = await http.patch<S["SettingsReply"]>("/v1/sending/accounts/{id}/settings", {
          path: { id },
          body: settings,
        });
        qc.setQueryData(key, r);
        void qc.invalidateQueries({ queryKey: ["accounts", id, "status"] });
      }}
      onReset={async () => {
        const r = await http.post<S["SettingsReply"]>("/v1/sending/accounts/{id}/settings/reset", {
          path: { id },
        });
        qc.setQueryData(key, r);
      }}
    />
  );
}
