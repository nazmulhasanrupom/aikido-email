import type { LucideIcon } from "lucide-react";
import { Database, ShieldCheck, Sparkles, Users, Webhook } from "lucide-react";

/** One entry per integration. A new one is an entry here and a page in `routes/`. */
export interface Integration {
  id: string;
  category: "Applications" | "AI models" | "Databases" | "CRM" | "Webhooks";
  name: string;
  description: string;
  icon: LucideIcon;
  /** `soon` has no page yet. */
  soon?: boolean;
}

export const CATEGORIES: Integration["category"][] = ["Applications", "AI models", "Databases", "CRM", "Webhooks"];

export const INTEGRATIONS: Integration[] = [
  {
    id: "reoon",
    category: "Applications",
    name: "Reoon Email Verifier",
    description: "Check if lead emails exist before you send.",
    icon: ShieldCheck,
  },
  {
    id: "ai",
    category: "AI models",
    name: "AI models",
    description: "Choose the models that write AI parts, whole sequences and map CSV columns.",
    icon: Sparkles,
  },
  {
    id: "databases",
    category: "Databases",
    name: "Databases",
    description: "Connect your database to import leads.",
    icon: Database,
    soon: true,
  },
  {
    id: "crm",
    category: "CRM",
    name: "CRM",
    description: "Send replies and interest to your CRM.",
    icon: Users,
    soon: true,
  },
  {
    id: "webhooks",
    category: "Webhooks",
    name: "Webhooks",
    description: "Tell your own tools when something happens.",
    icon: Webhook,
    soon: true,
  },
];
