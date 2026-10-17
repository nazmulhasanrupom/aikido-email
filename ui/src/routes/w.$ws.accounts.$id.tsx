import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link, Outlet } from "@tanstack/react-router";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http } from "@/api/http";
import { q } from "@/api/queries";
import { Button, ErrorNote, LinkTabs, Page, PageHeader, Skeleton } from "@/components/ui";
import { AccountStatusPill } from "@/features/accounts/parts";

export const Route = createFileRoute("/w/$ws/accounts/$id")({ component: AccountLayout });

function AccountLayout() {
  const { ws, id } = Route.useParams();
  const qc = useQueryClient();
  const accounts = useQuery(q.accounts);
  const status = useQuery(q.sendStatus(id));
  const account = accounts.data?.find((a) => a.id === id);
  const paused = status.data?.paused || account?.status === "paused";
  const toggle = useMutation({
    mutationFn: () =>
      http.post(`/v1/sending/accounts/{id}/${paused ? "resume" : "pause"}`, {
        path: { id },
        body: {},
      }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["accounts"] }),
    onError: (e) => toast.error(messageOf(e)),
  });
  if (accounts.isPending)
    return (
      <Page>
        <Skeleton />
      </Page>
    );
  if (accounts.isError)
    return (
      <Page>
        <ErrorNote error={accounts.error} />
      </Page>
    );
  if (!account)
    return (
      <Page>
        <p>This account does not exist (any more).</p>
        <Link to="/w/$ws/accounts" params={{ ws }} className="text-accent underline">
          Back to accounts
        </Link>
      </Page>
    );
  const p = { ws, id };
  return (
    <Page wide>
      <PageHeader
        title={account.email}
        sub={<AccountStatusPill status={account.status} paused={status.data?.paused} />}
        actions={
          <Button variant="secondary" busy={toggle.isPending} onClick={() => toggle.mutate()}>
            {paused ? "Resume" : "Pause"}
          </Button>
        }
      />
      <LinkTabs
        label="Account pages"
        items={[
          { to: "/w/$ws/accounts/$id", params: p, label: "Overview" },
          { to: "/w/$ws/accounts/$id/sending", params: p, label: "Sending" },
          { to: "/w/$ws/accounts/$id/reply-check", params: p, label: "Reply check" },
          { to: "/w/$ws/accounts/$id/activity", params: p, label: "Activity" },
        ]}
      />
      <Outlet />
    </Page>
  );
}
