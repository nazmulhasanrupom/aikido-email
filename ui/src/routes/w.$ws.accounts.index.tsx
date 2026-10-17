import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { Mail, MoreHorizontal } from "lucide-react";
import { useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http } from "@/api/http";
import { q } from "@/api/queries";
import { MenuContent, MenuItem, MenuRoot, MenuTrigger } from "@/components/shell/menu";
import {
  Button,
  ConfirmDialog,
  EmptyState,
  ErrorNote,
  Head,
  Page,
  PageHeader,
  Skeleton,
  Table,
  TD,
} from "@/components/ui";
import { AddAccount } from "@/features/accounts/AddAccount";
import { AccountStatusPill, ReadingCell } from "@/features/accounts/parts";
import { fmtDateTime } from "@/lib/format";
import { say } from "@/lib/words";

export const Route = createFileRoute("/w/$ws/accounts/")({ component: Accounts });

function Accounts() {
  const { ws } = Route.useParams();
  const accounts = useQuery(q.accounts);
  const [adding, setAdding] = useState(false);
  return (
    <Page wide>
      <PageHeader
        title="Email Accounts"
        sub="The mailboxes that send your emails."
        actions={<Button onClick={() => setAdding(true)}>Add account</Button>}
      />
      {accounts.isPending && <Skeleton />}
      {accounts.isError && <ErrorNote error={accounts.error} onRetry={() => accounts.refetch()} />}
      {accounts.data?.length === 0 && (
        <EmptyState
          icon={Mail}
          text="Connect an email account to start sending."
          action={<Button onClick={() => setAdding(true)}>Connect account</Button>}
        />
      )}
      {accounts.data && accounts.data.length > 0 && (
        <Table>
          <Head
            cols={["Email", "Provider", "Status", "Replies", "Today", "Warm-up", "Next send", ""]}
          />
          <tbody>
            {accounts.data.map((a) => (
              <Row key={a.id} ws={ws} account={a} />
            ))}
          </tbody>
        </Table>
      )}
      <AddAccount open={adding} onClose={() => setAdding(false)} />
    </Page>
  );
}

function Row({ ws, account: a }: { ws: string; account: import("@/api/http").S["AccountView"] }) {
  const qc = useQueryClient();
  const status = useQuery(q.sendStatus(a.id));
  const s = status.data;
  const [deleting, setDeleting] = useState(false);
  const act = useMutation({
    mutationFn: (what: "pause" | "resume") =>
      http.post(`/v1/sending/accounts/{id}/${what}`, { path: { id: a.id }, body: {} }),
    onSuccess: () => qc.invalidateQueries({ queryKey: ["accounts"] }),
    onError: (e) => toast.error(messageOf(e)),
  });
  const test = useMutation({
    mutationFn: () =>
      http.post<{ ok: boolean; detail: string }>("/v1/accounts/{id}/test", { path: { id: a.id } }),
    onSuccess: (r) => (r.ok ? toast.success(r.detail || "Login works.") : toast.error(r.detail)),
    onError: (e) => toast.error(messageOf(e)),
  });
  const wait = s?.wait
    ? say("wait", s.wait.reason)
    : s?.next_send_at
      ? fmtDateTime(s.next_send_at, s.timezone)
      : "";
  return (
    <tr className="border-t border-border hover:bg-surface-2/50">
      <td className={TD}>
        <Link
          to="/w/$ws/accounts/$id"
          params={{ ws, id: a.id }}
          className="font-medium hover:underline"
        >
          {a.email}
        </Link>
      </td>
      <td className={TD}>
        {a.provider === "gmail"
          ? "Google"
          : a.provider === "microsoft"
            ? "Microsoft"
            : "IMAP / SMTP"}
      </td>
      <td className={TD}>
        <AccountStatusPill status={a.status} paused={s?.paused} />
      </td>
      <td className={TD}>
        <ReadingCell account={a} />
      </td>
      <td className={TD}>{s ? `${s.sent_today} / ${s.today_limit}` : ""}</td>
      <td className={TD}>{s && s.warmup_day > 0 ? `Day ${s.warmup_day}` : "Off"}</td>
      <td className={TD}>{wait}</td>
      <td className={TD}>
        <MenuRoot>
          <MenuTrigger
            aria-label={`Actions for ${a.email}`}
            className="rounded p-1 hover:bg-surface-2"
          >
            <MoreHorizontal size={16} />
          </MenuTrigger>
          <MenuContent align="end">
            {s?.paused || a.status === "paused" ? (
              <MenuItem onSelect={() => act.mutate("resume")}>Resume</MenuItem>
            ) : (
              <MenuItem onSelect={() => act.mutate("pause")}>Pause</MenuItem>
            )}
            <MenuItem onSelect={() => test.mutate()}>Test login</MenuItem>
            <MenuItem onSelect={() => setDeleting(true)}>Delete</MenuItem>
          </MenuContent>
        </MenuRoot>
        <DeleteAccount open={deleting} onClose={() => setDeleting(false)} account={a} />
      </td>
    </tr>
  );
}

export function DeleteAccount({
  open,
  onClose,
  account,
  onDeleted,
}: {
  open: boolean;
  onClose: () => void;
  account: { id: string; email: string };
  onDeleted?: () => void;
}) {
  const qc = useQueryClient();
  return (
    <ConfirmDialog
      open={open}
      onClose={onClose}
      title="Delete this account?"
      text="Queued emails from this account are cancelled. Its sequences stop."
      typed={account.email}
      confirmLabel="Delete account"
      onConfirm={async () => {
        await http.delEmpty("/v1/accounts/{id}", { path: { id: account.id } });
        await qc.invalidateQueries({ queryKey: ["accounts"] });
        onDeleted?.();
      }}
    />
  );
}
