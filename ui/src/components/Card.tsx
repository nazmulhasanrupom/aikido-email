import type { HTMLAttributes } from "react";
import { cn } from "@/lib/cn";

export function Card({ className, ...rest }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn(
        "ui-card animate-rise-in rounded-(--radius-card) border border-border bg-surface p-6 shadow-(--shadow-card)",
        className,
      )}
      {...rest}
    />
  );
}
