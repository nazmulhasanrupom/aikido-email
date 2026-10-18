/** A plain score of one email, computed in the browser (UI update map v2, I3). */
export interface Score {
  words: number;
  links: number;
  spamWords: string[];
  questions: number;
  /** US school grade (Flesch-Kincaid). 0 when there is too little text. */
  grade: number;
  hints: { tone: "ok" | "warn"; text: string }[];
}

const URL_RE = /(?:https?:\/\/|www\.)\S+/gi;

function syllables(word: string): number {
  const w = word.toLowerCase().replace(/[^a-z]/g, "");
  if (!w) return 0;
  const groups = w.replace(/e$/, "").match(/[aeiouy]+/g);
  return Math.max(1, groups?.length ?? 1);
}

export function scoreEmail(text: string, spam: string[]): Score {
  const clean = text.replace(/\{\{[^}]*\}\}/g, "word").trim();
  const words = clean ? clean.split(/\s+/).filter(Boolean) : [];
  const sentences = Math.max(1, (clean.match(/[.!?]+(\s|$)/g) ?? []).length);
  const links = (clean.match(URL_RE) ?? []).length;
  const lower = clean.toLowerCase();
  const spamWords = spam.map((s) => s.toLowerCase()).filter((s) => s && lower.includes(s));
  const questions = (clean.match(/\?/g) ?? []).length;
  const sylls = words.reduce((n, w) => n + syllables(w), 0);
  const grade =
    words.length < 15
      ? 0
      : Math.max(0, Math.round((0.39 * (words.length / sentences) + 11.8 * (sylls / words.length) - 15.59) * 10) / 10);
  const hints: Score["hints"] = [];
  if (words.length > 125) hints.push({ tone: "warn", text: "Shorter emails get more replies. Aim for 50 to 125 words." });
  else if (words.length > 0 && words.length < 50) hints.push({ tone: "warn", text: "Very short. 50 to 125 words is a good size." });
  else if (words.length >= 50) hints.push({ tone: "ok", text: "A good length." });
  if (links > 1) hints.push({ tone: "warn", text: "Many links look like spam. Keep one at most, or none in the first email." });
  if (spamWords.length > 0) hints.push({ tone: "warn", text: `Spam words: ${spamWords.join(", ")}.` });
  if (words.length > 0 && questions === 0) hints.push({ tone: "warn", text: "Ask one clear question. It makes a reply easy." });
  if (questions > 2) hints.push({ tone: "warn", text: "Too many questions. Ask one." });
  if (grade > 9) hints.push({ tone: "warn", text: "Hard to read. Use shorter sentences and plain words." });
  return { words: words.length, links, spamWords, questions, grade, hints };
}
