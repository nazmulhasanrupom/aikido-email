import { useQuery } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { authStateQuery } from "@/api/auth";
import { Skeleton } from "@/components/ui";
import { Editor } from "@/features/campaigns/Editor";
import { campaignQuery } from "@/features/campaigns/hooks";

export const Route = createFileRoute("/w/$ws/campaigns/$id/editor")({ component: EditorPage });

function EditorPage() {
  const { id } = Route.useParams();
  const c = useQuery(campaignQuery(id));
  const auth = useQuery(authStateQuery);
  if (!c.data) return <Skeleton />;
  return (
    <>
      <p className="mb-2 hidden text-sm text-muted max-md:block">
        Use a bigger screen to edit the emails.
      </p>
      <Editor key={c.data.sequence.id} campaign={c.data} userEmail={auth.data?.user?.email ?? ""} />
    </>
  );
}
