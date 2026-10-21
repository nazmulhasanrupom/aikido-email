import type { LucideIcon } from "lucide-react";
import { useDesign } from "@/theme/theme";
import { Enso } from "./Enso";

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
  const design = useDesign();
  return (
    <div className="flex h-full flex-col items-center justify-center gap-3 px-6 text-center">
      {design === "aikido" ? (
        <span className="relative flex size-24 items-center justify-center">
          <Enso size={96} className="absolute inset-0 text-(--brand)" />
          <Icon size={26} className="relative" />
        </span>
      ) : (
        <div className="rounded-full bg-surface-2 p-3 text-muted">
          <Icon size={24} />
        </div>
      )}
      <h1 className="text-lg font-semibold">{title}</h1>
      <p className="max-w-md text-muted">{text}</p>
    </div>
  );
}
