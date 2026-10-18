import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute } from "@tanstack/react-router";
import { useState } from "react";
import { toast } from "sonner";
import { authStateKey, saveThemePreference } from "@/api/auth";
import { ApiProblem, messageOf, wordsFor } from "@/api/errors";
import { http, type S } from "@/api/http";
import { PasswordField } from "@/components/PasswordField";
import { Button, Card, Field, Head, Notice, Select, Skeleton, Table, TD } from "@/components/ui";
import { useUser } from "@/features/settings/useUser";
import { fmtDateTime } from "@/lib/format";
import { applyChoice, type ThemeChoice, useThemeChoice } from "@/theme/theme";

export const Route = createFileRoute("/w/$ws/settings/account")({ component: Account });

function Account() {
  const { user } = useUser();
  const qc = useQueryClient();
  const theme = useThemeChoice();
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
      <Card className="flex flex-col gap-3">
        <h2 className="font-semibold">Theme</h2>
        <Select
          label="Theme"
          hideLabel
          value={theme}
          onChange={async (v) => {
            applyChoice(v as ThemeChoice);
            try {
              await saveThemePreference(v);
            } catch (e) {
              toast.error(messageOf(e));
            }
          }}
          options={[
            { value: "system", label: "System" },
            { value: "light", label: "Light" },
            { value: "dark", label: "Dark" },
            { value: "aikido", label: "Aikido (deep red, Georgia font)" },
          ]}
        />
      </Card>
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
