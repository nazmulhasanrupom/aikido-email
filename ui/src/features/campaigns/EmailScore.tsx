import { useQuery } from "@tanstack/react-query";
import { q } from "@/api/queries";
import { scoreEmail } from "@/lib/emailScore";

/** Word count, links, spam words, questions and reading level, with plain hints. */
export function EmailScore({ text }: { text: string }) {
  const gen = useQuery(q.genSettings);
  const s = scoreEmail(text, gen.data?.settings.spam_words ?? []);
  if (s.words === 0) return null;
  return (
    <div className="rounded-(--radius-control) bg-surface-2 p-3 text-xs" aria-label="Email score">
      <p className="mb-1 flex flex-wrap gap-x-4 gap-y-1 text-muted">
        <span>{s.words} words</span>
        <span>{s.links} link{s.links === 1 ? "" : "s"}</span>
        <span>{s.questions} question{s.questions === 1 ? "" : "s"}</span>
        <span>{s.spamWords.length} spam word{s.spamWords.length === 1 ? "" : "s"}</span>
        {s.grade > 0 && <span>Reading level: grade {s.grade}</span>}
      </p>
      <ul className="flex flex-col gap-0.5">
        {s.hints.map((h) => (
          <li key={h.text} className={h.tone === "ok" ? "text-success" : "text-warning"}>
            {h.text}
          </li>
        ))}
      </ul>
    </div>
  );
}
