import { useEffect, useState } from "react";
import { cn } from "@/lib/cn";

const LABELS = ["Too easy", "Weak", "Okay", "Good", "Strong"];
const BAR = ["bg-danger", "bg-danger", "bg-warning", "bg-success", "bg-success"];

type Check = (password: string, inputs: string[]) => number;
let loaded: Promise<Check> | undefined;

/** The checker is big. Load it only when someone types a password. */
function loadChecker(): Promise<Check> {
  loaded ??= Promise.all([
    import("@zxcvbn-ts/core"),
    import("@zxcvbn-ts/language-common"),
    import("@zxcvbn-ts/language-en"),
  ]).then(([core, common, en]) => {
    const checker = new core.ZxcvbnFactory({
      translations: en.translations,
      graphs: common.adjacencyGraphs,
      dictionary: { ...common.dictionary, ...en.dictionary },
    });
    return (password, inputs) => checker.check(password, inputs).score;
  });
  return loaded;
}

interface Props {
  password: string;
  /** Name and email: a password that holds them is weak. */
  inputs?: string[];
}

/** A strength meter. The server makes the final check (score 3 or more, 12 or more characters). */
export function StrengthMeter({ password, inputs = [] }: Props) {
  const [score, setScore] = useState<number | null>(null);
  const key = inputs.join("|");

  // biome-ignore lint/correctness/useExhaustiveDependencies: `key` stands for `inputs`
  useEffect(() => {
    if (!password) {
      setScore(null);
      return;
    }
    let live = true;
    loadChecker().then((check) => {
      if (live) setScore(check(password, inputs.filter(Boolean)));
    });
    return () => {
      live = false;
    };
  }, [password, key]);

  if (score === null) return null;
  const short = password.length < 12;
  const shown = short ? Math.min(score, 1) : score;
  return (
    <div aria-live="polite" className="flex flex-col gap-1">
      <div className="flex gap-1" aria-hidden="true">
        {[0, 1, 2, 3, 4].map((i) => (
          <div
            key={i}
            className={cn("h-1 flex-1 rounded-full", i <= shown ? BAR[shown] : "bg-surface-2")}
          />
        ))}
      </div>
      <p className="text-xs text-muted">
        {short ? "Use at least 12 characters." : `Strength: ${LABELS[shown]}`}
        {!short && shown < 3 && ". Try a longer phrase of unrelated words."}
      </p>
    </div>
  );
}
