import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { type ReactNode, useState } from "react";
import { toast } from "sonner";
import { authStateKey, saveDesignPreference, saveThemePreference } from "@/api/auth";
import { ApiProblem, messageOf, wordsFor } from "@/api/errors";
import { http, type S } from "@/api/http";
import { PasswordField } from "@/components/PasswordField";
import { Button, Card, Field, Head, Notice, Select, Skeleton, Table, TD } from "@/components/ui";
import { useUser } from "@/features/settings/useUser";
import { fmtDateTime } from "@/lib/format";
import { Enso } from "@/components/Enso";
import { cn } from "@/lib/cn";
import {
  applyChoice,
  applyDesign,
  type DesignChoice,
  shownChoice,
  type ThemeChoice,
  themeOptions,
  useDesign,
  useThemeChoice,
} from "@/theme/theme";

export const Route = createFileRoute("/w/$ws/settings/account")({ component: Account });

function Account() {
  const { user } = useUser();
  const qc = useQueryClient();
  const [name, setName] = useState(user?.name ?? "");
  const [email, setEmail] = useState(user?.email ?? "");
  const [pwForEmail, setPwForEmail] = useState("");
  const [cur, setCur] = useState("");
  const [next, setNext] = useState("");
  const sessions = useQuery({
    queryKey: ["me", "sessions"],
    queryFn: () => http.get<S["SessionInfo"][]>("/v1/users/me/sessions"),
  });
  const profile = useMutation({
    mutationFn: () =>
      http.patch("/v1/users/me", {
        body: { name, ...(email !== user?.email ? { email, current_password: pwForEmail } : {}) },
      }),
    onSuccess: () => {
      toast.success("Saved.");
      void qc.invalidateQueries({ queryKey: authStateKey });
      setPwForEmail("");
    },
    onError: (e) => toast.error(e instanceof ApiProblem ? wordsFor(e) : messageOf(e)),
  });
  const password = useMutation({
    mutationFn: () =>
      http.postEmpty("/v1/users/me/password", {
        body: { current_password: cur, new_password: next },
      }),
    onSuccess: () => {
      toast.success("Password changed. Your other sessions ended.");
      setCur("");
      setNext("");
      void sessions.refetch();
    },
    onError: (e) => toast.error(e instanceof ApiProblem ? wordsFor(e) : messageOf(e)),
  });
  const end = useMutation({
    mutationFn: (id: string) => http.delEmpty("/v1/users/me/sessions/{id}", { path: { id } }),
    onSuccess: () => sessions.refetch(),
    onError: (e) => toast.error(messageOf(e)),
  });
  if (!user) return <Skeleton />;
  return (
    <div className="flex max-w-2xl flex-col gap-6">
      <Card className="flex flex-col gap-3">
        <h2 className="font-semibold">Profile</h2>
        <Field label="Name" value={name} onChange={(e) => setName(e.target.value)} />
        <Field
          label="Email"
          type="email"
          value={email}
          onChange={(e) => setEmail(e.target.value)}
        />
        {email !== user.email && (
          <PasswordField
            label="Current password (needed to change the email)"
            value={pwForEmail}
            onChange={(e) => setPwForEmail(e.target.value)}
            autoComplete="current-password"
          />
        )}
        <div>
          <Button busy={profile.isPending} onClick={() => profile.mutate()}>
            Save
          </Button>
        </div>
      </Card>
      <Card className="flex flex-col gap-3">
        <h2 className="font-semibold">Password</h2>
        <PasswordField
          label="Current password"
          value={cur}
          onChange={(e) => setCur(e.target.value)}
          autoComplete="current-password"
        />
        <PasswordField
          label="New password"
          value={next}
          onChange={(e) => setNext(e.target.value)}
          autoComplete="new-password"
        />
        <div>
          <Button
            busy={password.isPending}
            disabled={!cur || next.length < 12}
            onClick={() => password.mutate()}
          >
            Change password
          </Button>
        </div>
      </Card>
      <Appearance />
      <Card className="flex flex-col gap-3">
        <div className="flex items-center justify-between">
          <h2 className="font-semibold">Where you are logged in</h2>
          <Button
            variant="secondary"
            className="h-8"
            onClick={async () => {
              for (const s of sessions.data ?? []) if (!s.current) await end.mutateAsync(s.id);
            }}
          >
            Log out everywhere else
          </Button>
        </div>
        <Table>
          <Head cols={["Device", "Last seen", ""]} />
          <tbody>
            {(sessions.data ?? []).map((s) => (
              <tr key={s.id} className="border-t border-border">
                <td className={`${TD} max-w-xs truncate`}>
                  {s.user_agent || "Unknown"} <span className="text-muted">{s.ip}</span>{" "}
                  {s.current && <b>(this one)</b>}
                </td>
                <td className={TD}>{fmtDateTime(s.last_seen_at)}</td>
                <td className={TD}>
                  {!s.current && (
                    <Button variant="ghost" className="h-7" onClick={() => end.mutate(s.id)}>
                      Log out
                    </Button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </Table>
        <Notice>Lost your password? An admin can make you a reset link.</Notice>
      </Card>
    </div>
  );
}

/** The look of the app: the design (Generic, Indigo or Aikido) and the theme. Saved in this browser and in the account. */
function Appearance() {
  const design = useDesign();
  const theme = useThemeChoice();
  async function pickDesign(d: DesignChoice) {
    applyDesign(d);
    try {
      await saveDesignPreference(d);
    } catch (e) {
      toast.error(messageOf(e));
    }
  }
  async function pickTheme(v: string) {
    applyChoice(v as ThemeChoice);
    try {
      await saveThemePreference(v);
    } catch (e) {
      toast.error(messageOf(e));
    }
  }
  return (
    <Card className="flex flex-col gap-4">
      <div>
        <h2 className="font-semibold">Appearance</h2>
        <p className="text-sm text-muted">How the app looks for you. It does not change what others see.</p>
      </div>
      <fieldset className="flex flex-col gap-2">
        <legend className="mb-2 text-sm font-medium">Design</legend>
        <div className="grid gap-3 sm:grid-cols-3">
          <DesignTile
            value="generic"
            picked={design === "generic"}
            onPick={pickDesign}
            title="Generic"
            text="The first look: warm and simple, square corners, a light menu on the left."
            preview={<GenericPreview />}
          />
          <DesignTile
            value="indigo"
            picked={design === "indigo"}
            onPick={pickDesign}
            title="Indigo"
            text="The same layout in ink and paper, with an indigo accent and an ink menu."
            preview={<IndigoPreview />}
          />
          <DesignTile
            value="aikido"
            picked={design === "aikido"}
            onPick={pickDesign}
            title="Aikido"
            text="A new layout for daily work: pages on top, a Home page, a side panel and a live status bar."
            preview={<AikidoPreview />}
          />
        </div>
      </fieldset>
      <Select
        label="Theme"
        value={shownChoice(theme, design)}
        onChange={(v) => void pickTheme(v)}
        options={themeOptions(design)}
        help={THEME_HELP[design]}
      />
    </Card>
  );
}

const THEME_HELP: Record<DesignChoice, string> = {
  generic: "Crimson is the deep red theme with the Georgia font.",
  indigo: "Light is paper, Dark is ink. System follows your computer.",
  aikido: "Light is white and grey, Dark is near black. System follows your computer.",
};

function DesignTile({
  value,
  picked,
  onPick,
  title,
  text,
  preview,
}: {
  value: DesignChoice;
  picked: boolean;
  onPick: (d: DesignChoice) => void;
  title: string;
  text: string;
  preview: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-pressed={picked}
      onClick={() => onPick(value)}
      className={cn(
        "flex flex-col gap-2 rounded-(--radius-card) border p-2 text-left hover:bg-surface-2",
        picked ? "border-accent ring-2 ring-accent/30" : "border-border",
      )}
    >
      <span
        aria-hidden="true"
        className="relative flex h-28 overflow-hidden rounded-(--radius-control) border border-border"
      >
        {preview}
      </span>
      <span className="px-1">
        <span className="flex items-center justify-between font-medium">
          {title}
          {picked && <span className="text-xs font-normal text-accent">In use</span>}
        </span>
        <span className="text-xs text-muted">{text}</span>
      </span>
    </button>
  );
}

/*
 * Small pictures of the designs. The colors are each design's own, so a picture looks the same in every theme.
 */

/** A menu on the left and a card: Generic and Indigo share this layout. */
function RailPreview({
  c,
}: {
  c: { bg: string; rail: string; railLine: string; card: string; line: string; accent: string; mark: string; radius: number; button: number };
}) {
  return (
    <span className="flex flex-1" style={{ background: c.bg }}>
      <span
        className="flex w-1/4 flex-col gap-1.5 p-2"
        style={{ background: c.rail, borderRight: `1px solid ${c.railLine}` }}
      >
        <span className="h-2 w-2/3 rounded-sm" style={{ background: c.mark }} />
        <span className="mt-1 h-1.5 w-full rounded-sm" style={{ background: c.railLine }} />
        <span className="h-1.5 w-4/5 rounded-sm" style={{ background: c.railLine }} />
        <span className="h-1.5 w-full rounded-sm" style={{ background: c.railLine }} />
      </span>
      <span className="flex flex-1 flex-col gap-2 p-2.5">
        <span className="h-2.5 w-1/2 rounded-sm" style={{ background: "#14161b", opacity: 0.75 }} />
        <span
          className="flex flex-1 flex-col gap-1.5 border p-2"
          style={{ background: c.card, borderColor: c.line, borderRadius: c.radius }}
        >
          <span className="h-1.5 w-3/4 rounded-sm" style={{ background: c.line }} />
          <span className="h-1.5 w-1/2 rounded-sm" style={{ background: c.line }} />
          <span className="mt-auto h-3.5 w-12" style={{ background: c.accent, borderRadius: c.button }} />
        </span>
      </span>
    </span>
  );
}

function GenericPreview() {
  return (
    <RailPreview
      c={{ bg: "#f6f1e9", rail: "#fbf8f3", railLine: "#e3d9ca", card: "#fbf8f3", line: "#e3d9ca", accent: "#98502d", mark: "#98502d", radius: 4, button: 3 }}
    />
  );
}

function IndigoPreview() {
  return (
    <>
      <RailPreview
        c={{ bg: "#f3f1ec", rail: "#14161b", railLine: "#3a3f4b", card: "#fcfbf8", line: "#d6d1c5", accent: "#2b3a78", mark: "#ff7a5c", radius: 10, button: 999 }}
      />
      <Enso size={54} className="absolute -right-3 -bottom-3 text-[#c73a28] opacity-80" />
    </>
  );
}

/** The Aikido layout: the dark top bar, a section menu, panels, the side panel and the status bar. */
function AikidoPreview() {
  const line = "#e2e5ea";
  return (
    <span className="flex flex-1 flex-col" style={{ background: "#f4f5f7" }}>
      <span className="flex h-4 shrink-0 items-center gap-1.5 px-1.5" style={{ background: "#0e0f13" }}>
        <span className="size-2 rounded-[2px]" style={{ background: "#d92d20" }} />
        <span className="relative h-1 w-4 rounded-sm" style={{ background: "#f2f3f5" }}>
          <span className="absolute inset-x-0 -bottom-[5px] h-0.5" style={{ background: "#ff4d3d" }} />
        </span>
        <span className="h-1 w-4 rounded-sm" style={{ background: "#585d68" }} />
        <span className="h-1 w-3 rounded-sm" style={{ background: "#585d68" }} />
        <span className="ml-auto h-2 w-5 rounded-[2px]" style={{ background: "#f2f3f5" }} />
      </span>
      <span className="flex min-h-0 flex-1 gap-1.5 p-1.5">
        <span className="flex w-1/6 flex-col gap-1">
          <span className="h-2 rounded-[2px] bg-white" style={{ boxShadow: `inset 2px 0 0 #d92d20, 0 0 0 1px ${line}` }} />
          <span className="h-1 w-4/5 rounded-sm" style={{ background: "#c9ced6" }} />
          <span className="h-1 w-3/5 rounded-sm" style={{ background: "#c9ced6" }} />
        </span>
        <span className="flex flex-1 flex-col gap-1.5">
          {[0, 1].map((i) => (
            <span
              key={i}
              className="flex flex-1 flex-col gap-1 rounded-[3px] border bg-white p-1.5"
              style={{ borderColor: line }}
            >
              <span className="flex items-center gap-1">
                <span className="h-1.5 w-[2px]" style={{ background: "#d92d20" }} />
                <span className="h-1 w-1/3 rounded-sm" style={{ background: "#9aa0ad" }} />
              </span>
              <span className="h-1 w-3/4 rounded-sm" style={{ background: line }} />
            </span>
          ))}
        </span>
        <span className="flex w-1/4 flex-col gap-1 rounded-[3px] border bg-white p-1.5" style={{ borderColor: line }}>
          <span className="h-1.5 w-2/3 rounded-sm" style={{ background: "#0e0f13", opacity: 0.8 }} />
          <span className="h-1 w-full rounded-sm" style={{ background: line }} />
          <span className="h-1 w-4/5 rounded-sm" style={{ background: line }} />
        </span>
      </span>
      <span className="flex h-2.5 shrink-0 items-center gap-1 border-t bg-white px-1.5" style={{ borderColor: line }}>
        <span className="size-1 rounded-full" style={{ background: "#17734a" }} />
        <span className="h-0.5 w-6 rounded-sm" style={{ background: "#9aa0ad" }} />
      </span>
    </span>
  );
}
