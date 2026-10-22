import { useQuery } from "@tanstack/react-query";
import { Link } from "@tanstack/react-router";
import { q } from "@/api/queries";
import { Card } from "@/components/ui";

/** A short list of first steps. It hides when an email account exists. */
export function GetStarted({ ws }: { ws: string }) {
  const onboarding = useQuery(q.onboarding);
  // Wait for the facts, so the card does not flash for people who are set up.
  if (!onboarding.data || onboarding.data.facts.accounts > 0) return null;
  const link = "text-accent underline";
  return (
    <Card className="mb-5 flex flex-col gap-2" aria-label="Get started">
      <h2 className="font-semibold">Get started</h2>
      <p className="text-sm text-muted">Four steps to your first campaign.</p>
      <ul className="list-disc pl-5 text-sm">
        <li>
          <Link to="/w/$ws/accounts" params={{ ws }} className={link}>
            Connect an email account
          </Link>
        </li>
        <li>
          <Link to="/w/$ws/settings/integrations" params={{ ws }} className={link}>
            Add an AI model
          </Link>{" "}
          <span className="text-muted">(optional)</span>
        </li>
        <li>
          <Link to="/w/$ws/leads" params={{ ws }} className={link}>
            Import leads
          </Link>
        </li>
        <li>
          <Link to="/w/$ws/guide" params={{ ws }} className={link}>
            Open the guide
          </Link>
        </li>
      </ul>
    </Card>
  );
}
