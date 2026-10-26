import {
  Archive,
  Ban,
  Bot,
  CalendarClock,
  CalendarDays,
  Circle,
  Clock,
  FileJson,
  FileText,
  Filter,
  FlaskConical,
  GitBranch,
  Globe,
  Hourglass,
  Layers,
  type LucideIcon,
  Mail,
  Merge,
  MessageSquare,
  OctagonX,
  Pencil,
  Play,
  Plug,
  Reply,
  Rows3,
  Send,
  Sigma,
  Sparkles,
  Split,
  Tag,
  UserPlus,
  Users,
  Webhook,
  Workflow,
} from "lucide-react";
import type { Category } from "./types";

const BY_TYPE: Record<string, LucideIcon> = {
  "trigger.manual": Play,
  "trigger.schedule": Clock,
  "trigger.cron": CalendarClock,
  "trigger.webhook": Webhook,
  "trigger.lead_created": UserPlus,
  "trigger.lead_added_to_campaign": Send,
  "trigger.lead_added_to_list": Users,
  "trigger.reply_received": Reply,
  "trigger.email_sent": Mail,
  "trigger.lead_bounced": Ban,
  "trigger.lead_unsubscribed": Ban,
  "trigger.import_finished": FileText,
  "ai.agent": Sparkles,
  "http.request": Globe,
  "web.fetch_page": FileText,
  "aikido.api": Plug,
  "aikido.email.send": Mail,
  "aikido.email.reply": Reply,
  "aikido.generator.campaign_test": FlaskConical,
  "flow.if": GitBranch,
  "flow.switch": Split,
  "flow.filter": Filter,
  "flow.merge": Merge,
  "flow.wait": Hourglass,
  "flow.stop": OctagonX,
  "flow.noop": Circle,
  "data.set": Pencil,
  "data.split_out": Rows3,
  "data.aggregate": Sigma,
  "data.limit": Layers,
  "data.dedupe": Layers,
  "data.sort": Layers,
  "data.parse_json": FileJson,
  "data.date": CalendarDays,
};

export function iconFor(type: string): LucideIcon {
  const hit = BY_TYPE[type];
  if (hit) return hit;
  if (type.startsWith("trigger.")) return Play;
  if (type.startsWith("aikido.lead")) return Users;
  if (type.startsWith("aikido.campaign")) return Send;
  if (type.startsWith("aikido.reply")) return MessageSquare;
  if (type.startsWith("aikido.list")) return Tag;
  if (type.startsWith("aikido.suppression")) return Archive;
  if (type.startsWith("aikido.")) return Bot;
  return Workflow;
}

export const CATEGORY_LABEL: Record<Category, string> = {
  trigger: "Triggers",
  aikido: "AIKIDO",
  ai: "AI",
  web: "Web",
  flow: "Flow",
  data: "Data",
};

export const CATEGORY_ORDER: Category[] = ["trigger", "aikido", "ai", "web", "flow", "data"];

/** The accent is only on the small icon square, and muted in both themes. */
export const CATEGORY_ICON: Record<Category, string> = {
  trigger: "bg-accent-soft text-accent",
  aikido: "bg-text/10 text-text",
  ai: "bg-ai/15 text-ai",
  web: "bg-info/15 text-info",
  flow: "bg-surface-2 text-muted",
  data: "bg-surface-2 text-muted",
};

/** A short sentence for a trigger node, used in the Active dialog. */
export function describeTrigger(type: string, params: Record<string, unknown> | undefined): string {
  const p = params ?? {};
  switch (type) {
    case "trigger.schedule":
      return p.mode === "daily"
        ? `Every day at ${String(p.time ?? "?")}`
        : `Every ${String(p.every ?? 1)} ${String(p.unit ?? "hours")}`;
    case "trigger.cron":
      return `Cron: ${String(p.expression ?? "")}`;
    case "trigger.webhook":
      return `Webhook ${String(p.method ?? "POST")} /hooks/${String(p.path ?? "…")}`;
    case "trigger.manual":
      return "Manual start (Test workflow, or the run API)";
    default:
      return type.replace("trigger.", "").replace(/_/g, " ");
  }
}
