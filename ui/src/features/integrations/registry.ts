import type { LucideIcon } from "lucide-react";
import { Database, Sparkles, Users, Webhook } from "lucide-react";

/** One entry per integration. A new one is an entry here and a page in `routes/`. */
export interface Integration {
  id: string;
  category: "AI models" | "Databases" | "CRM" | "Webhooks";
  name: string;
  description: string;
  icon: LucideIcon;
  /** `soon` has no page yet. */
  soon?: boolean;
}

export const CATEGORIES: Integration["category"][] = ["AI models", "Databases", "CRM", "Webhooks"];

export const INTEGRATIONS: Integration[] = [
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
