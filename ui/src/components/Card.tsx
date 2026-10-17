import type { HTMLAttributes } from "react";
import { cn } from "@/lib/cn";

export function Card({ className, ...rest }: HTMLAttributes<HTMLDivElement>) {
  return (
    <div
      className={cn("rounded-(--radius-card) border border-border bg-surface p-6", className)}
      {...rest}
    />
  );
}
