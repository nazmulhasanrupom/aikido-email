import { useQueries, useQuery, useQueryClient } from "@tanstack/react-query";
import { createFileRoute, Link } from "@tanstack/react-router";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { messageOf } from "@/api/errors";
import { http, type S } from "@/api/http";
import { q } from "@/api/queries";
import {
  Button,
  Card,
  Check2,
  ConfirmDialog,
  Head,
  Notice,
  NumField,
  Pill,
  Select,
  Skeleton,
  Table,
  TD,
} from "@/components/ui";
import { campaignQuery } from "@/features/campaigns/hooks";
import { browserTimezone, timezones } from "@/lib/format";
import { say } from "@/lib/words";

export const Route = createFileRoute("/w/$ws/campaigns/$id/settings")({ component: Settings });

const DAY = ["", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"];

function Settings() {
  const { id, ws } = Route.useParams();
  const qc = useQueryClient();
  const camp = useQuery(campaignQuery(id));
  const accounts = useQuery(q.accounts);
  const genCamp = useQuery({
    queryKey: ["generator", "campaign", id],
    queryFn: () =>
      http.get<S["CampaignGeneratorReply"]>("/v1/generator/campaigns/{id}", { path: { id } }),
  });
  const c = camp.data;
  const [picked, setPicked] = useState<string[]>([]);
  const [pick, setPick] = useState<string>("least_loaded");
  const [daily, setDaily] = useState(50);
  const [tz, setTz] = useState("UTC");
  const [company, setCompany] = useState(false);
  const [unverified, setUnverified] = useState(false);
  const [review, setReview] = useState("default");
  const [budget, setBudget] = useState(0);
  useEffect(() => {
    if (!c) return;
    setPicked(c.accounts.map((a) => a.id));
    setPick(c.account_pick);
    setDaily(c.daily_new_leads);
    setTz(c.timezone);
    setCompany(c.stop_company_on_reply);
    setUnverified(c.allow_unverified);
  }, [c]);
  useEffect(() => {
    if (genCamp.data) {
      setReview(
        genCamp.data.review_required == null
          ? "default"
          : genCamp.data.review_required
            ? "always"
            : "never",
      );
      setBudget(genCamp.data.token_budget ?? 0);
    }
  }, [genCamp.data]);
  const poolSettings = useQueries({
    queries: picked.map((aid) => ({
      queryKey: ["accounts", aid, "sending-settings"],
      queryFn: () =>
        http.get<S["SettingsReply"]>("/v1/sending/accounts/{id}/settings", { path: { id: aid } }),
    })),
  });
  const [archive, setArchive] = useState(false);
  if (!c) return <Skeleton />;
  const done = async (what: string, run: () => Promise<unknown>) => {
    try {
      await run();
      toast.success(`${what} saved.`);
      await qc.invalidateQueries({ queryKey: ["campaigns"] });
    } catch (e) {
      toast.error(messageOf(e));
    }
  };
  const zones = timezones();
  return (
    <div className="flex max-w-3xl flex-col gap-6">
      <Card className="flex flex-col gap-3">
        <h2 className="font-semibold">Accounts to use</h2>
        <ul className="flex flex-col gap-1.5">
          {(accounts.data ?? []).map((a) => {
            const healthy = c.accounts.find((x) => x.id === a.id)?.healthy;
            return (
              <li key={a.id}>
                <Check2
                  label={
                    <span className="flex items-center gap-2">
                      {a.email}{" "}
                      <Pill tone={a.status === "active" ? "success" : "warning"}>
                        {say("account", a.status)}
                      </Pill>{" "}
                      {!a.can_read && <Pill tone="warning">Cannot read replies</Pill>}{" "}
                      {healthy === false && <Pill tone="danger">Not healthy</Pill>}
                    </span>
                  }
                  checked={picked.includes(a.id)}
                  onChange={(on) =>
                    setPicked(on ? [...picked, a.id] : picked.filter((x) => x !== a.id))
                  }
                />
              </li>
            );
          })}
        </ul>
        <Select
          label="How to pick an account for a new lead"
          value={pick}
          onChange={setPick}
          options={[
            { value: "least_loaded", label: "Least busy" },
            { value: "round_robin", label: "Take turns" },
          ]}
          help="Each lead gets one account for the whole sequence."
        />
        <div>
          <Button
            onClick={() =>
              done("Accounts", () =>
                http.patch("/v1/campaigns/{id}", {
                  path: { id },
                  body: { account_ids: picked, account_pick: pick },
                }),
              )
            }
          >
            Save
          </Button>
        </div>
      </Card>

      <Card className="flex flex-col gap-3">
        <h2 className="font-semibold">Schedule and limits</h2>
        <NumField label="New leads per day" value={daily} min={1} max={5000} onChange={setDaily} />
        <Select
          label="A day starts at midnight in"
          value={tz}
          onChange={setTz}
          options={(zones.includes(tz) ? zones : [tz, ...zones]).map((z) => ({
            value: z,
            label: z,
          }))}
          help={`Your browser says ${browserTimezone()}.`}
        />
        <Table>
          <Head cols={["Account", "Days", "Hours", "Per day", "Time zone", ""]} />
          <tbody>
            {picked.map((aid, i) => {
              const s = poolSettings[i]?.data?.settings;
              const email = accounts.data?.find((a) => a.id === aid)?.email;
              return (
                <tr key={aid} className="border-t border-border">
                  <td className={TD}>{email}</td>
                  <td className={TD}>{s?.send_days.map((d) => DAY[d]).join(" ")}</td>
                  <td className={TD}>{s ? `${s.window_start} to ${s.window_end}` : ""}</td>
                  <td className={TD}>{s?.daily_limit}</td>
                  <td className={TD}>{s?.timezone}</td>
                  <td className={TD}>
                    <Link
                      to="/w/$ws/accounts/$id/sending"
                      params={{ ws, id: aid }}
                      className="text-accent underline"
                    >
                      Edit
                    </Link>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </Table>
        {c.warnings.map((w) => (
          <Notice key={w} tone="warning">
            {w}
          </Notice>
        ))}
        <div>
          <Button
            onClick={() =>
              done("Schedule", () =>
                http.patch("/v1/campaigns/{id}", {
                  path: { id },
                  body: { daily_new_leads: daily, timezone: tz },
                }),
              )
            }
          >
            Save
          </Button>
        </div>
      </Card>

      <Card className="flex flex-col gap-3">
        <h2 className="font-semibold">Advanced</h2>
        <Check2 label="Stop sending on reply" help="This is always on." checked disabled />
        <Check2
          label="Stop everyone at the same company when one person replies"
          help="Free email domains like gmail.com are never one company."
          checked={company}
          onChange={setCompany}
        />
        <Check2
          label="Send to emails we could not check"
          checked={unverified}
          onChange={setUnverified}
        />
        <div>
          <Button
            onClick={() =>
              done("Advanced", () =>
                http.patch("/v1/campaigns/{id}", {
                  path: { id },
                  body: { stop_company_on_reply: company, allow_unverified: unverified },
                }),
              )
            }
          >
            Save
          </Button>
        </div>
        <hr className="border-border" />
        <Select
          label="AI review"
          value={review}
          onChange={setReview}
          options={[
            { value: "default", label: "Use the default" },
            { value: "always", label: "Always review" },
            { value: "never", label: "Never review" },
          ]}
        />
        <NumField
          label="AI token limit for this campaign"
          value={budget}
          min={0}
          help={`0 means no limit. Used so far: ${genCamp.data?.tokens_used ?? 0}.`}
          onChange={setBudget}
        />
        <div>
          <Button
            onClick={() =>
              done("AI settings", () =>
                http.patch("/v1/generator/campaigns/{id}", {
                  path: { id },
                  body: {
                    review_required: review === "default" ? null : review === "always",
                    token_budget: budget || null,
                  },
                }),
              )
            }
          >
            Save
          </Button>
        </div>
      </Card>

      <Card className="flex flex-col gap-2 border-danger/40">
        <h2 className="font-semibold text-danger">Danger</h2>
        <div>
          <Button variant="danger" onClick={() => setArchive(true)}>
            Archive campaign
          </Button>
        </div>
      </Card>
      <ConfirmDialog
        open={archive}
        onClose={() => setArchive(false)}
        title="Archive this campaign?"
        typed={c.name}
        confirmLabel="Archive campaign"
        text="Pending leads are skipped and the sequence stops."
        onConfirm={async () => {
          await http.post("/v1/campaigns/{id}/archive", { path: { id } });
          await qc.invalidateQueries({ queryKey: ["campaigns"] });
        }}
      />
    </div>
  );
}
