import { useQueryClient } from "@tanstack/react-query";
import { useEffect, useState } from "react";
import { ApiProblem, messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import { Button, Field, Modal, Notice } from "@/components/ui";
import { PROBLEM_WORDS } from "@/lib/words";
import { type Server, ServerBlock } from "./AddAccount";

const fromHost = (h: S["HostInfo"] | null | undefined, port: number, tls: S["TlsMode"]): Server => ({
  host: h?.host ?? "",
  port: String(h?.port ?? port),
  tls: h?.tls ?? tls,
  username: "",
  password: "",
});

/** Change the display name and the SMTP / IMAP login of an account. */
export function EditLogin({
  open,
  onClose,
  account,
}: {
  open: boolean;
  onClose: () => void;
  account: S["AccountView"];
}) {
  const qc = useQueryClient();
  const [name, setName] = useState(account.display_name);
  const [smtp, setSmtp] = useState<Server>(() => fromHost(account.smtp, 587, "starttls"));
  const [imap, setImap] = useState<Server>(() => fromHost(account.imap, 993, "implicit"));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  useEffect(() => {
    if (!open) return;
    setName(account.display_name);
    setSmtp(fromHost(account.smtp, 587, "starttls"));
    setImap(fromHost(account.imap, 993, "implicit"));
    setError(null);
  }, [open, account]);

  /** Only the fields that changed (a username or password counts when it is not empty). */
  function patchOf(now: Server, old: S["HostInfo"] | null | undefined): S["ServerPatch"] | null {
    const p: S["ServerPatch"] = {};
    if (now.host.trim() !== (old?.host ?? "")) p.host = now.host.trim();
    if (Number(now.port) !== old?.port) p.port = Number(now.port);
    if (now.tls !== old?.tls) p.tls = now.tls;
    if (now.username.trim()) p.username = now.username.trim();
    if (now.password) p.password = now.password;
    return Object.keys(p).length ? p : null;
  }

  async function submit() {
    setBusy(true);
    setError(null);
    try {
      const body: S["PatchAccount"] = {};
      if (name.trim() !== account.display_name) body.display_name = name.trim();
      const s = patchOf(smtp, account.smtp);
      if (s) body.smtp = s;
      if (account.imap || imap.host.trim()) {
        const i = patchOf(imap, account.imap);
        if (i) body.imap = i;
      }
      if (Object.keys(body).length === 0) {
        onClose();
        return;
      }
      await http.patch<S["AccountView"]>("/v1/accounts/{id}", {
        path: { id: account.id },
        body,
      });
      await Promise.all([
        qc.invalidateQueries({ queryKey: q.accounts.queryKey }),
        qc.invalidateQueries({ queryKey: ["accounts", account.id] }),
      ]);
      onClose();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  const problem =
    error instanceof ApiProblem
      ? error.message || PROBLEM_WORDS[error.code]
      : error
        ? messageOf(error)
        : null;
  return (
    <Modal open={open} onClose={onClose} title="Edit login" size="md">
      <div className="flex flex-col gap-4">
        <Field
          label="Name shown to people"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
        <ServerBlock
          title="Sending (SMTP)"
          value={smtp}
          onChange={setSmtp}
          ports={[465, 587]}
          userHint="Keep the saved username"
          passwordHint="Keep the saved password"
        />
        <ServerBlock
          title="Reading (IMAP)"
          value={imap}
          onChange={setImap}
          ports={[993, 143]}
          userHint="Keep the saved username"
          passwordHint="Keep the saved password"
        />
        <p className="text-xs text-muted">
          Leave a username or password empty to keep the saved one. A changed server is tested
          before it is saved.
        </p>
        {problem && <Notice tone="danger">{problem}</Notice>}
        <div className="flex gap-2">
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button busy={busy} onClick={submit}>
            Test and save
          </Button>
        </div>
      </div>
    </Modal>
  );
}
