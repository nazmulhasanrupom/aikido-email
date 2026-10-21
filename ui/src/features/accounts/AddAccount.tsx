import { useQueryClient } from "@tanstack/react-query";
import { Mail } from "lucide-react";
import { useEffect, useRef, useState } from "react";
import { ApiProblem, messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import { Button, CopyButton, Field, Modal, Notice, Select } from "@/components/ui";
import { PROBLEM_WORDS } from "@/lib/words";

const PRESETS: Record<string, { smtp: [string, number]; imap: [string, number] }> = {
  Zoho: { smtp: ["smtp.zoho.com", 465], imap: ["imap.zoho.com", 993] },
  Yandex: { smtp: ["smtp.yandex.com", 465], imap: ["imap.yandex.com", 993] },
  Fastmail: { smtp: ["smtp.fastmail.com", 465], imap: ["imap.fastmail.com", 993] },
  "Namecheap Private Email": {
    smtp: ["mail.privateemail.com", 465],
    imap: ["mail.privateemail.com", 993],
  },
};

type Mode = "choose" | "google" | "microsoft" | "other";

/** Connect an inbox: Google, Microsoft (a popup) or any SMTP/IMAP server (a form). */
export function AddAccount({
  open,
  onClose,
  onAdded,
  start,
}: {
  open: boolean;
  onClose: () => void;
  onAdded?: (a: S["AccountView"]) => void;
  /** Skip the choice and open this flow first. */
  start?: "gmail" | "microsoft" | "other";
}) {
  const first: Mode =
    start === "gmail" ? "google" : start === "microsoft" ? "microsoft" : (start ?? "choose");
  const [mode, setMode] = useState<Mode>(first);
  useEffect(() => {
    if (open) setMode(first);
  }, [open, first]);
  const title =
    mode === "choose"
      ? "Add an email account"
      : mode === "other"
        ? "Other email (IMAP / SMTP)"
        : "Connect";
  return (
    <Modal open={open} onClose={onClose} title={title} size="md">
      {mode === "choose" && (
        <div className="grid gap-3">
          <Choice
            title="Google"
            text="Gmail and Google Workspace"
            onClick={() => setMode("google")}
          />
          <Choice
            title="Microsoft"
            text="Outlook and Office 365"
            onClick={() => setMode("microsoft")}
          />
          <Choice
            title="Other"
            text="Any email host with IMAP and SMTP"
            onClick={() => setMode("other")}
          />
        </div>
      )}
      {(mode === "google" || mode === "microsoft") && (
        <OAuthFlow
          provider={mode === "google" ? "gmail" : "microsoft"}
          onBack={() => setMode("choose")}
          onAdded={(a) => {
            onAdded?.(a);
            onClose();
          }}
        />
      )}
      {mode === "other" && (
        <SmtpForm
          onBack={() => setMode("choose")}
          onAdded={(a) => {
            onAdded?.(a);
            onClose();
          }}
        />
      )}
    </Modal>
  );
}

function Choice({ title, text, onClick }: { title: string; text: string; onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="flex items-center gap-3 rounded-(--radius-card) border border-border p-4 text-left hover:bg-surface-2"
    >
      <Mail size={20} aria-hidden="true" className="text-muted" />
      <span className="flex flex-col">
        <span className="font-medium">{title}</span>
        <span className="text-xs text-muted">{text}</span>
      </span>
    </button>
  );
}

function OAuthFlow({
  provider,
  onBack,
  onAdded,
}: {
  provider: "gmail" | "microsoft";
  onBack: () => void;
  onAdded: (a: S["AccountView"]) => void;
}) {
  const qc = useQueryClient();
  const [state, setState] = useState<"idle" | "waiting" | "failed">("idle");
  const [error, setError] = useState<ApiProblem | string | null>(null);
  const known = useRef<Map<string, string>>(new Map());
  const timer = useRef<ReturnType<typeof setInterval> | null>(null);
  const label = provider === "gmail" ? "Google" : "Microsoft";

  useEffect(() => {
    const stop = () => timer.current && clearInterval(timer.current);
    const onMessage = (e: MessageEvent) => {
      if (e.origin !== window.location.origin) return;
      const d = e.data as { type?: string; ok?: boolean; code?: string; email?: string } | null;
      if (d?.type !== "aikido:oauth") return;
      stop();
      if (d.ok) void finish();
      else {
        setState("failed");
        setError(d.code ?? "The login did not finish.");
      }
    };
    window.addEventListener("message", onMessage);
    return () => {
      window.removeEventListener("message", onMessage);
      stop();
    };
  }, []);

  async function finish() {
    const list = await http.get<S["AccountView"][]>("/v1/accounts");
    await qc.invalidateQueries({ queryKey: q.accounts.queryKey });
    // a reconnect updates the same row, so also accept the newest one
    const added =
      list.find((a) => isNewOrChanged(a, known.current)) ??
      [...list].sort((a, b) => b.updated_at.localeCompare(a.updated_at))[0];
    if (added) onAdded(added);
  }

  async function start() {
    setError(null);
    try {
      const before = await http.get<S["AccountView"][]>("/v1/accounts");
      known.current = new Map(before.map((a) => [a.id, a.updated_at]));
      const { auth_url } = await http.post<S["OAuthStartReply"]>(
        "/v1/accounts/oauth/{provider}/start",
        { path: { provider } },
      );
      window.open(auth_url, "aikido-oauth", "width=500,height=650");
      setState("waiting");
      // the popup may lose its link to us; look at the list every 2 seconds for 5 minutes
      const until = Date.now() + 5 * 60_000;
      timer.current = setInterval(async () => {
        if (Date.now() > until) {
          if (timer.current) clearInterval(timer.current);
          setState("failed");
          setError("We did not see the account. Try again.");
          return;
        }
        const list = await http.get<S["AccountView"][]>("/v1/accounts").catch(() => []);
        if (list.some((a) => isNewOrChanged(a, known.current))) {
          if (timer.current) clearInterval(timer.current);
          void finish();
        }
      }, 2000);
    } catch (e) {
      setState("failed");
      setError(e instanceof ApiProblem ? e : messageOf(e));
    }
  }

  const cfg = useConfig();
  const notReady = error instanceof ApiProblem && error.code === "oauth_not_configured";
  return (
    <div className="flex flex-col gap-4">
      <p className="text-muted">
        A {label} window opens. Log in there and allow access. This window then closes.
      </p>
      {state === "waiting" && <Notice>Waiting for {label}...</Notice>}
      {notReady && (
        <Notice tone="warning">
          <p className="mb-2">{PROBLEM_WORDS.oauth_not_configured}</p>
          <p>
            Set the {label} client id and secret in the server settings (see the API guide, section
            7). In the {label} console, use this redirect address:
          </p>
          <div className="mt-2 flex items-center gap-2">
            <code className="min-w-0 flex-1 truncate rounded bg-surface-2 px-2 py-1 font-mono text-xs">
              {cfg?.[provider] ?? ""}
            </code>
            {cfg?.[provider] && <CopyButton text={cfg[provider] as string} />}
          </div>
          {provider === "gmail" && (
            <p className="mt-2 text-xs">
              Reading replies uses a Google restricted scope. A public app needs a Google review. An
              Internal Workspace app does not.
            </p>
          )}
        </Notice>
      )}
      {error && !notReady && (
        <Notice tone="danger">{typeof error === "string" ? error : messageOf(error)}</Notice>
      )}
      <div className="flex gap-2">
        <Button variant="secondary" onClick={onBack}>
          Back
        </Button>
        <Button busy={state === "waiting"} onClick={start}>
          Connect {label}
        </Button>
      </div>
    </div>
  );
}

/** A new account, or an old one that was updated (a reconnect) since we looked. */
function isNewOrChanged(a: S["AccountView"], before: Map<string, string>): boolean {
  const old = before.get(a.id);
  return old === undefined || old !== a.updated_at;
}

function useConfig(): S["RedirectUris"] | undefined {
  const [uris, setUris] = useState<S["RedirectUris"]>();
  useEffect(() => {
    http
      .get<S["UiConfig"]>("/v1/ui/config")
      .then((c) => setUris(c.redirect_uris))
      .catch(() => {});
  }, []);
  return uris;
}

export interface Server {
  host: string;
  port: string;
  tls: S["TlsMode"];
  username: string;
  password: string;
}
export const blank = (port: number, tls: S["TlsMode"]): Server => ({
  host: "",
  port: String(port),
  tls,
  username: "",
  password: "",
});

function SmtpForm({
  onBack,
  onAdded,
}: {
  onBack: () => void;
  onAdded: (a: S["AccountView"]) => void;
}) {
  const qc = useQueryClient();
  const [email, setEmail] = useState("");
  const [name, setName] = useState("");
  const [smtp, setSmtp] = useState<Server>(blank(587, "starttls"));
  const [imap, setImap] = useState<Server>(blank(993, "implicit"));
  const [useImap, setUseImap] = useState(true);
  const [same, setSame] = useState(true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  function preset(key: string) {
    const p = PRESETS[key];
    if (!p) return;
    setSmtp((s) => ({ ...s, host: p.smtp[0], port: String(p.smtp[1]), tls: "implicit" }));
    setImap((s) => ({ ...s, host: p.imap[0], port: String(p.imap[1]), tls: "implicit" }));
  }

  const toInput = (s: Server, fallbackUser: string): S["ServerInput"] => ({
    host: s.host.trim(),
    port: Number(s.port),
    tls: s.tls,
    username: s.username.trim() || fallbackUser,
    password: s.password,
  });

  async function submit() {
    setBusy(true);
    setError(null);
    try {
      const smtpIn = toInput(smtp, email);
      const imapIn = useImap
        ? same
          ? {
              ...toInput(smtp, email),
              host: imap.host.trim(),
              port: Number(imap.port),
              tls: imap.tls,
            }
          : toInput(imap, email)
        : null;
      const body = {
        email: email.trim(),
        display_name: name.trim() || null,
        smtp: smtpIn,
        imap: imapIn,
      };
      const account = await http.post<S["AccountView"]>("/v1/accounts/smtp", { body });
      await qc.invalidateQueries({ queryKey: q.accounts.queryKey });
      onAdded(account);
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  }

  const problem =
    error instanceof ApiProblem
      ? (PROBLEM_WORDS[error.code] ?? error.message)
      : error
        ? messageOf(error)
        : null;
  return (
    <div className="flex flex-col gap-4">
      <div className="grid grid-cols-2 gap-3">
        <Field
          label="Email address"
          type="email"
          value={email}
          onChange={(e) => setEmail(e.target.value)}
        />
        <Field
          label="Name shown to people (optional)"
          value={name}
          onChange={(e) => setName(e.target.value)}
        />
      </div>
      <Select
        label="Fill in a common host"
        value=""
        options={[
          { value: "", label: "Choose..." },
          ...Object.keys(PRESETS).map((k) => ({ value: k, label: k })),
        ]}
        onChange={preset}
        help="This fills host and port only."
      />
      <ServerBlock
        title="Sending (SMTP)"
        value={smtp}
        onChange={setSmtp}
        ports={[465, 587]}
        userHint={email}
      />
      <label className="flex items-center gap-2 text-sm">
        <input type="checkbox" checked={useImap} onChange={(e) => setUseImap(e.target.checked)} />
        Also read replies (IMAP). Needed to see replies and stop follow-ups.
      </label>
      {useImap && (
        <>
          <label className="flex items-center gap-2 text-sm">
            <input type="checkbox" checked={same} onChange={(e) => setSame(e.target.checked)} />
            Same login as SMTP
          </label>
          <ServerBlock
            title="Reading (IMAP)"
            value={imap}
            onChange={setImap}
            ports={[993, 143]}
            userHint={email}
            hideLogin={same}
          />
        </>
      )}
      {problem && <Notice tone="danger">{problem}</Notice>}
      <div className="flex gap-2">
        <Button variant="secondary" onClick={onBack}>
          Back
        </Button>
        <Button busy={busy} onClick={submit} disabled={!email || !smtp.host || !smtp.password}>
          Test and save
        </Button>
      </div>
    </div>
  );
}

export function ServerBlock({
  title,
  value,
  onChange,
  ports,
  userHint,
  hideLogin,
  passwordHint,
}: {
  title: string;
  value: Server;
  onChange: (s: Server) => void;
  ports: number[];
  userHint: string;
  hideLogin?: boolean;
  passwordHint?: string;
}) {
  const set = (p: Partial<Server>) => onChange({ ...value, ...p });
  return (
    <fieldset className="flex flex-col gap-3 rounded-(--radius-card) border border-border p-3">
      <legend className="px-1 text-sm font-medium">{title}</legend>
      <div className="grid grid-cols-3 gap-3">
        <div className="col-span-2">
          <Field label="Host" value={value.host} onChange={(e) => set({ host: e.target.value })} />
        </div>
        <Field
          label="Port"
          list={`ports-${title}`}
          value={value.port}
          onChange={(e) => set({ port: e.target.value })}
        />
        <datalist id={`ports-${title}`}>
          {ports.map((p) => (
            <option key={p} value={p} />
          ))}
        </datalist>
      </div>
      <Select
        label="Security"
        value={value.tls}
        options={[
          { value: "implicit", label: "SSL/TLS" },
          { value: "starttls", label: "STARTTLS" },
          { value: "none", label: "None (this computer only)" },
        ]}
        onChange={(tls) => set({ tls: tls as S["TlsMode"] })}
      />
      {!hideLogin && (
        <div className="grid grid-cols-2 gap-3">
          <Field
            label="Username"
            placeholder={userHint}
            value={value.username}
            onChange={(e) => set({ username: e.target.value })}
          />
          <Field
            label="Password"
            type="password"
            autoComplete="new-password"
            placeholder={passwordHint}
            value={value.password}
            onChange={(e) => set({ password: e.target.value })}
            help="Many providers need an app password."
          />
        </div>
      )}
      {hideLogin && null}
    </fieldset>
  );
}
