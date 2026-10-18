import { AI_PRESETS } from "./aiPresets";

/** Starter kit for "New campaign" (UI update map v2, I7). Three template sequences and three AI prompt sets. */
export interface Starter {
  id: string;
  name: string;
  help: string;
  kind: "template" | "ai";
  steps: {
    delay_amount: number;
    subject: string;
    body_text: string;
    ai_user_prompt?: string;
  }[];
  system?: string;
}

const SIGN = "\n\nBest,\n{{sender_name}}";

export const TEMPLATE_STARTERS: Starter[] = [
  {
    id: "t-short",
    name: "Short intro, 3 emails",
    help: "A short hello, a nudge, a last note.",
    kind: "template",
    steps: [
      {
        delay_amount: 0,
        subject: "Quick question, {{first_name|there}}",
        body_text: `Hi {{first_name|there}},\n\nI help teams like {{company_name|yours}} with [what you do]. Is this something you look at?${SIGN}`,
      },
      { delay_amount: 3, subject: "", body_text: `Hi {{first_name|there}},\n\nJust checking you saw my note. A reply of "not now" is fine too.${SIGN}` },
      { delay_amount: 4, subject: "", body_text: `Hi {{first_name|there}},\n\nI will stop here. If this becomes useful, you know where I am.${SIGN}` },
    ],
  },
  {
    id: "t-problem",
    name: "Problem, then offer",
    help: "Name a problem first, then the offer.",
    kind: "template",
    steps: [
      {
        delay_amount: 0,
        subject: "{{company_name|Your team}} and [problem]",
        body_text: `Hi {{first_name|there}},\n\nMost {{job_title|teams}} we talk to struggle with [problem]. We [offer in one sentence].\n\nWorth a short chat?${SIGN}`,
      },
      { delay_amount: 3, subject: "", body_text: `Hi {{first_name|there}},\n\nOne example: [short result with a number]. Happy to show how.${SIGN}` },
      { delay_amount: 5, subject: "", body_text: `Hi {{first_name|there}},\n\nShould I close this out?${SIGN}` },
    ],
  },
  {
    id: "t-referral",
    name: "Who is the right person?",
    help: "Ask for the right contact. A soft first step.",
    kind: "template",
    steps: [
      {
        delay_amount: 0,
        subject: "Who handles [topic] at {{company_name|your company}}?",
        body_text: `Hi {{first_name|there}},\n\nI am not sure you are the right person. Who looks after [topic] at {{company_name|your company}}?${SIGN}`,
      },
      { delay_amount: 4, subject: "", body_text: `Hi {{first_name|there}},\n\nA quick nudge. Could you point me to the right person?${SIGN}` },
    ],
  },
];

export const AI_STARTERS: Starter[] = AI_PRESETS.map((p, i) => ({
  id: `a-${i}`,
  name: `AI: ${p.name}`,
  help: p.help,
  kind: "ai" as const,
  system: p.system,
  steps: [0, 1, 2].map((n) => ({
    delay_amount: n === 0 ? 0 : 3,
    subject: n === 0 ? "Quick question" : "",
    // The template text is not used in AI mode, but a step needs a body.
    body_text: "(written by the AI)",
    ai_user_prompt: p.steps[Math.min(n, p.steps.length - 1)] ?? "",
  })),
}));

export const STARTERS: Starter[] = [...TEMPLATE_STARTERS, ...AI_STARTERS];
