import { forwardRef, type InputHTMLAttributes, type ReactNode, useId } from "react";
import { cn } from "@/lib/cn";

interface FieldProps extends InputHTMLAttributes<HTMLInputElement> {
  label: string;
  help?: ReactNode;
  error?: string | undefined;
  /** Shown to the right of the input, inside it (for example a show/hide button). */
  adornment?: ReactNode;
}

/** A labelled text input. Every input has a label, and an error is read out with the input. */
export const Field = forwardRef<HTMLInputElement, FieldProps>(function Field(
  { label, help, error, adornment, className, id, ...rest },
  ref,
) {
  const auto = useId();
  const inputId = id ?? auto;
  const helpId = `${inputId}-help`;
  const errorId = `${inputId}-error`;
  return (
    <div className="flex flex-col gap-1.5">
      <label htmlFor={inputId} className="text-sm font-medium">
        {label}
      </label>
      <div className="relative">
        <input
          ref={ref}
          id={inputId}
          aria-invalid={error ? true : undefined}
          aria-describedby={cn(help && helpId, error && errorId) || undefined}
          className={cn(
            "h-9 w-full rounded-(--radius-control) border bg-surface-2 px-3 text-sm placeholder:text-muted",
            error ? "border-danger" : "border-border",
            adornment && "pr-10",
            className,
          )}
          {...rest}
        />
        {adornment && (
          <div className="absolute inset-y-0 right-1 flex items-center">{adornment}</div>
        )}
      </div>
      {help && (
        <p id={helpId} className="text-xs text-muted">
          {help}
        </p>
      )}
      {error && (
        <p id={errorId} role="alert" className="text-xs text-danger">
          {error}
        </p>
      )}
    </div>
  );
});
