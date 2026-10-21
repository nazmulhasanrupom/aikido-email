import { createFileRoute } from "@tanstack/react-router";
import { Card, Check2, Notice } from "@/components/ui";
import { useDeveloperMode } from "@/features/developer/api";

export const Route = createFileRoute("/w/$ws/settings/developer")({ component: Developer });

function Developer() {
  const dev = useDeveloperMode();
  if (!dev.isAdmin) return <Notice tone="warning">Only an owner or an admin can use the developer tools.</Notice>;
  return (
    <div className="flex max-w-2xl flex-col gap-4">
      <Card className="flex flex-col gap-3">
        <h2 className="font-semibold">Developer mode</h2>
        <Check2
          label="Show the developer panel"
          help="Off by default. When on, a panel at the bottom of every page shows what the server is doing."
          checked={dev.on}
          onChange={(v) => dev.set(v)}
          disabled={dev.saving}
        />
        <ul className="list-disc space-y-1 pl-5 text-sm text-muted">
          <li>
            <strong className="text-text">Logs:</strong> a live log of everything the server does: sends, replies, steps, imports, tracking, requests, and every failure. Search, filter by level, pause, copy.
            It keeps the last 5000 lines in memory and is empty after a restart.
          </li>
          <li>
            <strong className="text-text">SQL:</strong> run SQL on the database. Read only by default. You can preview changes (rolled back) or run and save them.
          </li>
          <li>
            <strong className="text-text">Tools:</strong> server facts, counts of queued, stuck and failed work, and buttons that run a part of the server now.
          </li>
        </ul>
      </Card>
      <Notice tone="warning">
        The SQL console is raw access to your whole database, including inbox text and encrypted secrets. Only owners and admins can use it. Changes you save cannot be undone. Take a
        backup first. The server can turn all of this off with <code>AIKIDO_DEV_TOOLS=false</code>.
      </Notice>
    </div>
  );
}
