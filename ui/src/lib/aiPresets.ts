/** Starter prompt sets for AI mode (UI update map v2, section 4.2 and I7). */
export interface PromptPreset {
  name: string;
  help: string;
  system: string;
  /** The user prompt of step 1, then of each follow up. The last one is reused for more follow ups. */
  steps: string[];
}

export const AI_PRESETS: PromptPreset[] = [
  {
    name: "Short and direct",
    help: "Three or four lines. One clear ask.",
    system:
      "You write short cold emails for {{sender_name}}. Plain words, no hype, no flattery. Three to four sentences. End with one clear question.",
    steps: [
      "Write the first email to {{first_name|there}}, {{job_title|}} at {{company_name|their company}}.\nWhat we offer: {{brief}}\nNotes about the lead: {{lead_notes}}",
      "Write a very short follow up to {{first_name|there}}. Two sentences. Do not repeat the first email.\nEarlier emails:\n{{previous_emails}}",
    ],
  },
  {
    name: "Problem, then offer",
    help: "Names a problem the lead may have, then the offer.",
    system:
      "You write cold emails for {{sender_name}}. Start with one problem that people in the lead's role often have. Then give the offer in one sentence. Then ask a small question. Under 100 words.",
    steps: [
      "Write the first email to {{first_name|there}} at {{company_name|their company}}.\nOffer: {{brief}}\nWhat we know: {{lead_notes}}",
      "Write a follow up that adds one new fact or example. Keep it under 60 words.\nEarlier emails:\n{{previous_emails}}",
    ],
  },
  {
    name: "Friendly referral ask",
    help: "Asks who is the right person to talk to.",
    system:
      "You write warm, humble emails for {{sender_name}}. You are not sure the lead is the right person. Ask who handles this at their company. Under 70 words.",
    steps: [
      "Write the first email to {{first_name|there}} at {{company_name|their company}}. Say in one sentence what we do: {{brief}}. Ask who is the right person to talk to.",
      "Write a polite nudge. One or two sentences.\nEarlier emails:\n{{previous_emails}}",
    ],
  },
];
