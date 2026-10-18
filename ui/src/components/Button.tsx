import type { ButtonHTMLAttributes } from "react";
import { cn } from "@/lib/cn";

type Variant = "primary" | "secondary" | "ghost" | "danger";

const VARIANTS: Record<Variant, string> = {
  primary:
    "bg-accent text-accent-fg shadow-(--shadow-card) hover:brightness-110 hover:shadow-(--shadow-pop)",
  secondary:
    "border border-border bg-surface text-text shadow-(--shadow-card) hover:border-text/20 hover:bg-surface-2",
  ghost: "text-text hover:bg-surface-2",
  danger: "bg-danger text-accent-fg shadow-(--shadow-card) hover:brightness-110",
};

interface Props extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant;
  busy?: boolean;
}

export function Button({
  variant = "primary",
  busy,
  className,
  children,
  disabled,
  ...rest
}: Props) {
  return (
    <button
      type="button"
      disabled={disabled || busy}
      className={cn(
        "inline-flex h-9 select-none items-center justify-center gap-2 rounded-(--radius-control) px-3.5 text-sm font-medium transition-[color,background-color,border-color,box-shadow,filter,transform] duration-150 ease-(--ease-out-soft) active:scale-[0.97] disabled:pointer-events-none disabled:opacity-60",
        VARIANTS[variant],
        className,
      )}
      {...rest}
    >
      {busy ? (
        <>
          <span
            aria-hidden="true"
            className="size-3.5 animate-spin rounded-full border-2 border-current border-r-transparent"
          />
          Working...
        </>
      ) : (
        children
      )}
    </button>
  );
}
