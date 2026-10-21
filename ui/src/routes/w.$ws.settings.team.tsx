import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { toast } from "sonner";
import { ApiProblem, messageOf, wordsFor } from "@/api/errors";
import { http, type S } from "@/api/http";
import { PasswordField } from "@/components/PasswordField";
import {
  Button,
  ConfirmDialog,
  CopyButton,
  ErrorNote,
  Field,
  Head,
  Modal,
  Notice,
  Pill,
  Select,
  Skeleton,
  Table,
  TD,
} from "@/components/ui";
import { useUser } from "@/features/settings/useUser";
import { fmtAgo, fmtDate } from "@/lib/format";

export const Route = createFileRoute("/w/$ws/settings/team")({ component: Team });

function Team() {
  const { user, isAdmin, isOwner } = useUser();
  const qc = useQueryClient();
  const users = useQuery({
    queryKey: ["team", "users"],
    enabled: isAdmin,
    queryFn: () => http.get<S["UserOut"][]>("/v1/users"),
  });
  const invites = useQuery({
    queryKey: ["team", "invites"],
    enabled: isAdmin,
    queryFn: () => http.get<S["InviteOut"][]>("/v1/users/invites"),
  });
  const [inviting, setInviting] = useState(false);
  const [email, setEmail] = useState("");
  const [role, setRole] = useState("member");
  const [link, setLink] = useState<string | null>(null);
  const [remove, setRemove] = useState<S["UserOut"] | null>(null);
  const [transfer, setTransfer] = useState<S["UserOut"] | null>(null);
  const [pw, setPw] = useState("");
  const refresh = () => {
    void qc.invalidateQueries({ queryKey: ["team"] });
  };
  const patch = useMutation({
    mutationFn: ({ id, body }: { id: string; body: Record<string, unknown> }) =>
      http.patch("/v1/users/{id}", { path: { id }, body }),
    onSuccess: refresh,
    onError: (e) => toast.error(e instanceof ApiProblem ? wordsFor(e) : messageOf(e)),
  });
  const invite = useMutation({
    mutationFn: () =>
      http.post<S["NewInviteReply"]>("/v1/users/invites", { body: { email, role } }),
    onSuccess: (r) => {
      setInviting(false);
      setEmail("");
      setLink(r.link);
      refresh();
    },
  });
  const resetLink = useMutation({
    mutationFn: (id: string) =>
      http.post<S["ResetLinkReply"]>("/v1/users/{id}/reset-link", { path: { id } }),
    onSuccess: (r) => setLink(r.link),
    onError: (e) => toast.error(messageOf(e)),
  });
  if (!isAdmin)
    return <Notice tone="warning">Only an owner or an admin can manage the team.</Notice>;
  return (
    <div className="flex flex-col gap-6">
      <div>
        <Button onClick={() => setInviting(true)}>Invite someone</Button>
      </div>
      {users.isPending && <Skeleton rows={3} />}
      {users.isError && <ErrorNote error={users.error} />}
      {users.data && (
        <Table>
          <Head cols={["Name", "Email", "Role", "Status", "Last login", ""]} />
          <tbody>
            {users.data.map((u) => {
              const self = u.id === user?.id;
              const locked = !!u.locked_until && new Date(u.locked_until) > new Date();
              return (
                <tr key={u.id} className="border-t border-border">
                  <td className={`${TD} font-medium`}>
                    {u.name}
                    {self ? " (you)" : ""}
                  </td>
                  <td className={TD}>{u.email}</td>
                  <td className={TD}>
                    {u.role === "owner" || self ? (
                      u.role
                    ) : (
                      <select
                        aria-label={`Role of ${u.name}`}
                        value={u.role}
                        onChange={(e) => patch.mutate({ id: u.id, body: { role: e.target.value } })}
                        className="h-8 rounded border border-border bg-(--input-bg) px-1.5"
                      >
                        <option value="admin">admin</option>
                        <option value="member">member</option>
                      </select>
                    )}
                  </td>
                  <td className={TD}>
                    {locked ? (
                      <Pill tone="danger">Locked</Pill>
                    ) : (
                      <Pill tone={u.status === "active" ? "success" : "neutral"}>{u.status}</Pill>
                    )}
                  </td>
                  <td className={TD}>{fmtAgo(u.last_login_at)}</td>
                  <td className={TD}>
                    {!self && u.role !== "owner" && (
                      <>
                        {locked && (
                          <Button
                            variant="ghost"
                            className="h-7"
                            onClick={() => patch.mutate({ id: u.id, body: { unlock: true } })}
                          >
                            Unlock
                          </Button>
                        )}
                        <Button
                          variant="ghost"
                          className="h-7"
                          onClick={() =>
                            patch.mutate({
                              id: u.id,
                              body: { status: u.status === "active" ? "disabled" : "active" },
                            })
                          }
                        >
                          {u.status === "active" ? "Disable" : "Enable"}
                        </Button>
                        <Button
                          variant="ghost"
                          className="h-7"
                          onClick={() => resetLink.mutate(u.id)}
                        >
                          Reset link
                        </Button>
                        {isOwner && u.role === "admin" && (
                          <Button variant="ghost" className="h-7" onClick={() => setTransfer(u)}>
                            Make owner
                          </Button>
                        )}
                        <Button variant="ghost" className="h-7" onClick={() => setRemove(u)}>
                          Remove
                        </Button>
                      </>
                    )}
                  </td>
                </tr>
              );
            })}
          </tbody>
        </Table>
      )}
      {(invites.data?.length ?? 0) > 0 && (
        <section>
          <h2 className="mb-2 font-semibold">Waiting invites</h2>
          <Table>
            <Head cols={["Email", "Role", "Expires", ""]} />
            <tbody>
              {invites.data?.map((i) => (
                <tr key={i.id} className="border-t border-border">
                  <td className={TD}>{i.email}</td>
                  <td className={TD}>{i.role}</td>
                  <td className={TD}>{fmtDate(i.expires_at)}</td>
                  <td className={TD}>
                    <Button
                      variant="ghost"
                      className="h-7"
                      onClick={async () => {
                        await http.delEmpty("/v1/users/invites/{id}", { path: { id: i.id } });
                        refresh();
                      }}
                    >
                      Revoke
                    </Button>
                  </td>
                </tr>
              ))}
            </tbody>
          </Table>
        </section>
      )}
      <Modal
        open={inviting}
        onClose={() => setInviting(false)}
        size="sm"
        title="Invite someone"
        footer={
          <>
            <Button variant="secondary" onClick={() => setInviting(false)}>
              Cancel
            </Button>
            <Button busy={invite.isPending} disabled={!email} onClick={() => invite.mutate()}>
              Make invite link
            </Button>
          </>
        }
      >
        <div className="flex flex-col gap-3">
          <Field
            label="Email"
            type="email"
            value={email}
            onChange={(e) => setEmail(e.target.value)}
          />
          <Select
            label="Role"
            value={role}
            onChange={setRole}
            options={[
              { value: "member", label: "Member" },
              { value: "admin", label: "Admin" },
            ]}
          />
          {invite.isError && <ErrorNote error={invite.error} />}
        </div>
      </Modal>
      <Modal
        open={!!link}
        onClose={() => setLink(null)}
        title="Copy this link"
        footer={<Button onClick={() => setLink(null)}>Done</Button>}
      >
        <div className="flex flex-col gap-3">
          <Notice tone="warning">
            You will not see this link again. It works once and expires in 7 days. We do not send
            emails yet, so send it yourself.
          </Notice>
          <code className="break-all rounded bg-surface-2 p-3 text-sm">{link}</code>
          {link && (
            <div>
              <CopyButton text={link} />
            </div>
          )}
        </div>
      </Modal>
      <ConfirmDialog
        open={!!remove}
        onClose={() => setRemove(null)}
        title="Remove this person?"
        text="They lose access at once."
        confirmLabel="Remove"
        onConfirm={async () => {
          await http.delEmpty("/v1/users/{id}", { path: { id: (remove as S["UserOut"]).id } });
          refresh();
        }}
      />
      <Modal
        open={!!transfer}
        onClose={() => {
          setTransfer(null);
          setPw("");
        }}
        size="sm"
        title="Make this person the owner"
        footer={
          <>
            <Button variant="secondary" onClick={() => setTransfer(null)}>
              Cancel
            </Button>
            <Button
              variant="danger"
              disabled={!pw}
              onClick={async () => {
                try {
                  await http.postEmpty("/v1/users/{id}/transfer-ownership", {
                    path: { id: (transfer as S["UserOut"]).id },
                    body: { password: pw },
                  });
                  toast.success("Done.");
                  setTransfer(null);
                  setPw("");
                  refresh();
                  window.location.reload();
                } catch (e) {
                  toast.error(e instanceof ApiProblem ? wordsFor(e) : messageOf(e));
                }
              }}
            >
              Transfer ownership
            </Button>
          </>
        }
      >
        <PasswordField
          label="Your password"
          value={pw}
          onChange={(e) => setPw(e.target.value)}
          autoComplete="current-password"
          help="You become an admin. There is one owner."
        />
      </Modal>
    </div>
  );
}
