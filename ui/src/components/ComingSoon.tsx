import type { LucideIcon } from "lucide-react";

/** A page that is not built yet. It keeps the menu item working. */
export function ComingSoon({
  icon: Icon,
  title,
  text,
}: {
  icon: LucideIcon;
  title: string;
  text: string;
}) {
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 px-6 text-center">
      <div className="rounded-full bg-surface-2 p-3 text-muted">
        <Icon size={24} />
      </div>
      <h1 className="text-lg font-semibold">{title}</h1>
      <p className="max-w-md text-muted">{text}</p>
    </div>
  );
}
