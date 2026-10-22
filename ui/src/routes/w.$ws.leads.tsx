import { createFileRoute, useNavigate } from "@tanstack/react-router";
import { Page, PageHeader, SectionLayout, Tabs } from "@/components/ui";
import { AllLeads, type LeadSearch } from "@/features/leads/AllLeads";
import { LeadDrawer } from "@/features/leads/LeadDrawer";
import { ListView } from "@/features/leads/ListView";
import { BlocklistTab, FieldsTab, ImportsTab, ListsTab } from "@/features/leads/Tabs";

type TabId = "all" | "lists" | "fields" | "imports" | "blocklist";
type Search = LeadSearch & { tab?: TabId; lead?: string; list?: string };
const str = (v: unknown) => (typeof v === "string" && v ? v : undefined);

export const Route = createFileRoute("/w/$ws/leads")({
  validateSearch: (s: Record<string, unknown>): Search => {
    const tab = ["all", "lists", "fields", "imports", "blocklist"].includes(String(s.tab))
      ? (s.tab as TabId)
      : undefined;
    return {
      ...(tab ? { tab } : {}),
      ...Object.fromEntries(
        [
          "q",
          "list_id",
          "tag",
          "campaign_id",
          "contact_state",
          "interest",
          "verification",
          "suppressed",
          "verification_status",
          "lead",
          "list",
        ]
          .map((k) => [k, str(s[k])])
          .filter(([, v]) => v),
      ),
    };
  },
  component: Leads,
});

function Leads() {
  const { ws } = Route.useParams();
  const search = Route.useSearch();
  const navigate = useNavigate();
  const tab = search.tab ?? "all";
  return (
    <Page wide>
      <PageHeader title="Leads" sub="Everyone you email, in one place." />
      <SectionLayout
        nav={
          <Tabs
            rail
            label="Leads pages"
            value={tab}
            onChange={(t) =>
              navigate({ to: "/w/$ws/leads", params: { ws }, search: t === "all" ? {} : { tab: t } })
            }
            items={[
              { value: "all", label: "All leads" },
              { value: "lists", label: "Lists" },
              { value: "fields", label: "Custom fields" },
              { value: "imports", label: "Imports" },
              { value: "blocklist", label: "Blocklist" },
            ]}
          />
        }
      >
        {tab === "all" && <AllLeads search={search} ws={ws} />}
        {tab === "lists" && !search.list && (
          <ListsTab
            onOpen={(id) =>
              navigate({ to: "/w/$ws/leads", params: { ws }, search: { tab: "lists", list: id } })
            }
          />
        )}
        {tab === "lists" && search.list && (
          <ListView
            id={search.list}
            onBack={() => navigate({ to: "/w/$ws/leads", params: { ws }, search: { tab: "lists" } })}
          />
        )}
        {tab === "fields" && <FieldsTab />}
        {tab === "imports" && <ImportsTab />}
        {tab === "blocklist" && <BlocklistTab />}
      </SectionLayout>
      <LeadDrawer
        id={search.lead ?? null}
        ws={ws}
        onClose={() =>
          navigate({ to: "/w/$ws/leads", params: { ws }, search: { ...search, lead: undefined } })
        }
      />
    </Page>
  );
}
