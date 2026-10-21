import { Link } from "@tanstack/react-router";
import { Check, Copy, type LucideIcon } from "lucide-react";
import {
  forwardRef,
  type ReactNode,
  type SelectHTMLAttributes,
  type TextareaHTMLAttributes,
  useEffect,
  useId,
  useRef,
  useState,
} from "react";
import { messageOf } from "@/api/errors";
import { cn } from "@/lib/cn";
import { useDesign } from "@/theme/theme";
import { Button } from "../Button";
import { Enso } from "../Enso";

export { Button } from "../Button";
export { Card } from "../Card";
export { Field } from "../Field";

/* ---------- small things ---------- */

type Tone = "neutral" | "success" | "warning" | "danger" | "info" | "ai" | "accent";
const TONES: Record<Tone, string> = {
  neutral: "bg-surface-2 text-muted",
  success: "bg-success/15 text-success",
  warning: "bg-warning/15 text-warning",
  danger: "bg-danger/15 text-danger",
  info: "bg-info/15 text-info",
  ai: "bg-ai/15 text-ai",
  accent: "bg-accent-soft text-text",
};

export function Pill({
  tone = "neutral",
  dot,
  children,
}: {
  tone?: Tone;
  dot?: boolean;
  children: ReactNode;
}) {
  return (
    <span
      className={cn(
        "ui-pill inline-flex items-center gap-1.5 whitespace-nowrap rounded-full px-2 py-0.5 text-xs font-medium",
        TONES[tone],
      )}
    >
      {dot && <span aria-hidden="true" className="size-1.5 rounded-full bg-current" />}
      {children}
    </span>
  );
}

export function PageHeader({
  title,
  sub,
  actions,
}: {
  title: ReactNode;
  sub?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <div className="mb-5 flex flex-wrap items-start justify-between gap-3">
      <div className="min-w-0">
        <h1 className="ui-page-title truncate text-(length:--page-title) leading-(--page-title-leading) font-semibold">
          {title}
        </h1>
        {sub && <p className="mt-0.5 text-muted">{sub}</p>}
      </div>
      {actions && <div className="flex flex-wrap items-center gap-2">{actions}</div>}
    </div>
  );
}

export function Page({ children, wide }: { children: ReactNode; wide?: boolean }) {
  return (
    <div className={cn("mx-auto px-6 py-6", wide ? "max-w-[1400px]" : "max-w-5xl")}>{children}</div>
  );
}

export function EmptyState({
  icon: Icon,
  title,
  text,
  action,
}: {
  icon?: LucideIcon;
  title?: string;
  text: string;
  action?: ReactNode;
}) {
  const design = useDesign();
  return (
    <div className="animate-rise-in flex flex-col items-center gap-3 rounded-(--radius-card) border border-dashed border-border px-6 py-12 text-center">
      {design === "aikido" ? (
        // the Aikido design draws the icon inside a brush circle
        <span className="relative flex size-16 items-center justify-center">
          <Enso size={64} className="absolute inset-0 text-(--brand) opacity-85" />
          {Icon && <Icon size={20} className="relative text-text" aria-hidden="true" />}
        </span>
      ) : (
        Icon && <Icon size={22} className="text-muted" aria-hidden="true" />
      )}
      {title && <h2 className="font-semibold">{title}</h2>}
      <p className="max-w-md text-muted">{text}</p>
      {action}
    </div>
  );
}

export function Skeleton({ rows = 5, className }: { rows?: number; className?: string }) {
  return (
    <div className={cn("flex flex-col gap-2", className)} aria-busy="true" aria-label="Loading">
      {Array.from({ length: rows }, (_, i) => (
        <div
          key={i}
          className="skeleton h-10 rounded-(--radius-control)"
          style={{ animationDelay: `${i * 80}ms` }}
        />
      ))}
    </div>
  );
}

export function ErrorNote({ error, onRetry }: { error: unknown; onRetry?: () => void }) {
  return (
    <div
      role="alert"
      className="animate-rise-in flex items-center justify-between gap-3 rounded-(--radius-control) border border-danger/40 bg-danger/10 px-3 py-2 text-danger"
    >
      <span>{messageOf(error)}</span>
      {onRetry && (
        <Button variant="secondary" onClick={onRetry} className="h-7">
          Try again
        </Button>
      )}
    </div>
  );
}

export function Notice({
  tone = "info",
  children,
}: {
  tone?: "info" | "warning" | "danger" | "success";
  children: ReactNode;
}) {
  const t = {
    info: "border-info/40 bg-info/10",
    warning: "border-warning/40 bg-warning/10",
    danger: "border-danger/40 bg-danger/10",
    success: "border-success/40 bg-success/10",
  }[tone];
  return (
    <div className={cn("animate-fade-in rounded-(--radius-control) border px-3 py-2 text-sm", t)}>
      {children}
    </div>
  );
}

export function CopyButton({ text, label = "Copy" }: { text: string; label?: string }) {
  const [done, setDone] = useState(false);
  return (
    <Button
      variant="secondary"
      className="h-8"
      onClick={async () => {
        try {
          await navigator.clipboard.writeText(text);
          setDone(true);
          setTimeout(() => setDone(false), 1500);
        } catch {
          // the browser refused; the text is still on screen to select
        }
      }}
    >
      {done ? <Check size={14} /> : <Copy size={14} />} {done ? "Copied" : label}
    </Button>
  );
}

/* ---------- form controls ---------- */

const CONTROL =
  "w-full rounded-(--radius-control) border border-border bg-(--input-bg) px-3 text-sm placeholder:text-muted disabled:opacity-60";

interface SelectProps extends Omit<SelectHTMLAttributes<HTMLSelectElement>, "onChange"> {
  label: string;
  help?: ReactNode;
  error?: string | undefined;
  options: { value: string; label: string }[];
  onChange?: (value: string) => void;
  hideLabel?: boolean;
}

export const Select = forwardRef<HTMLSelectElement, SelectProps>(function Select(
  { label, help, error, options, onChange, hideLabel, className, id, ...rest },
  ref,
) {
  const auto = useId();
  const sid = id ?? auto;
  return (
    <div className="flex flex-col gap-1.5">
      <label htmlFor={sid} className={cn("text-sm font-medium", hideLabel && "sr-only")}>
        {label}
      </label>
      <select
        ref={ref}
        id={sid}
        aria-invalid={error ? true : undefined}
        className={cn(CONTROL, "h-9", error && "border-danger", className)}
        onChange={(e) => onChange?.(e.target.value)}
        {...rest}
      >
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
      {help && <p className="text-xs text-muted">{help}</p>}
      {error && (
        <p role="alert" className="text-xs text-danger">
          {error}
        </p>
      )}
    </div>
  );
});

interface AreaProps extends TextareaHTMLAttributes<HTMLTextAreaElement> {
  label: string;
  help?: ReactNode;
  error?: string | undefined;
}

export const TextArea = forwardRef<HTMLTextAreaElement, AreaProps>(function TextArea(
  { label, help, error, className, id, ...rest },
  ref,
) {
  const auto = useId();
  const tid = id ?? auto;
  return (
    <div className="flex flex-col gap-1.5">
      <label htmlFor={tid} className="text-sm font-medium">
        {label}
      </label>
      <textarea
        ref={ref}
        id={tid}
        aria-invalid={error ? true : undefined}
        className={cn(CONTROL, "min-h-20 py-2", error && "border-danger", className)}
        {...rest}
      />
      {help && <p className="text-xs text-muted">{help}</p>}
      {error && (
        <p role="alert" className="text-xs text-danger">
          {error}
        </p>
      )}
    </div>
  );
});

export function Check2({
  label,
  help,
  checked,
  onChange,
  disabled,
}: {
  label: ReactNode;
  help?: ReactNode;
  checked: boolean;
  onChange?: (v: boolean) => void;
  disabled?: boolean;
}) {
  const id = useId();
  return (
    <div className="flex items-start gap-2.5">
      <input
        id={id}
        type="checkbox"
        role="switch"
        aria-checked={checked}
        checked={checked}
        disabled={disabled}
        onChange={(e) => onChange?.(e.target.checked)}
        className="mt-0.5 size-4 accent-(--accent)"
      />
      <label htmlFor={id} className="flex flex-col text-sm">
        <span>{label}</span>
        {help && <span className="text-xs text-muted">{help}</span>}
      </label>
    </div>
  );
}

/** A number input that keeps a string while typing and gives a number up. */
export function NumField({
  label,
  value,
  onChange,
  min,
  max,
  help,
  suffix,
}: {
  label: string;
  value: number;
  onChange: (n: number) => void;
  min?: number;
  max?: number;
  help?: ReactNode;
  suffix?: string;
}) {
  const id = useId();
  return (
    <div className="flex flex-col gap-1.5">
      <label htmlFor={id} className="text-sm font-medium">
        {label}
      </label>
      <div className="flex items-center gap-2">
        <input
          id={id}
          type="number"
          value={Number.isFinite(value) ? value : ""}
          min={min}
          max={max}
          onChange={(e) => onChange(e.target.value === "" ? Number.NaN : Number(e.target.value))}
          className={cn(CONTROL, "h-9")}
        />
        {suffix && <span className="shrink-0 text-muted">{suffix}</span>}
      </div>
      {help && <p className="text-xs text-muted">{help}</p>}
    </div>
  );
}

/** A list of words typed one by one (keyword lists, domains, tags). */
export function TagInput({
  label,
  values,
  onChange,
  placeholder,
  help,
}: {
  label: string;
  values: string[];
  onChange: (v: string[]) => void;
  placeholder?: string;
  help?: ReactNode;
}) {
  const [draft, setDraft] = useState("");
  const id = useId();
  function add(raw: string) {
    const next = raw
      .split(/[\n,]/)
      .map((s) => s.trim())
      .filter((s) => s && !values.includes(s));
    if (next.length) onChange([...values, ...next]);
    setDraft("");
  }
  return (
    <div className="flex flex-col gap-1.5">
      <label htmlFor={id} className="text-sm font-medium">
        {label}
      </label>
      <div className="flex flex-wrap gap-1.5 rounded-(--radius-control) border border-border bg-(--input-bg) p-1.5">
        {values.map((v) => (
          <span
            key={v}
            className="inline-flex items-center gap-1 rounded bg-(--chip-bg) px-2 py-0.5 text-xs"
          >
            {v}
            <button
              type="button"
              aria-label={`Remove ${v}`}
              className="text-muted hover:text-text"
              onClick={() => onChange(values.filter((x) => x !== v))}
            >
              &times;
            </button>
          </span>
        ))}
        <input
          id={id}
          value={draft}
          placeholder={placeholder ?? "Type and press Enter"}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" || e.key === ",") {
              e.preventDefault();
              add(draft);
            } else if (e.key === "Backspace" && !draft && values.length) {
              onChange(values.slice(0, -1));
            }
          }}
          onBlur={() => draft && add(draft)}
          className="min-w-32 flex-1 bg-transparent px-1 text-sm outline-none"
        />
      </div>
      {help && <p className="text-xs text-muted">{help}</p>}
    </div>
  );
}

/* ---------- dialogs ---------- */

interface ModalProps {
  open: boolean;
  onClose: () => void;
  title: string;
  children: ReactNode;
  footer?: ReactNode;
  size?: "sm" | "md" | "lg" | "xl";
  side?: boolean;
}

/** A dialog on the browser's own `<dialog>`: focus is kept inside and Esc closes it. */
export function Modal({ open, onClose, title, children, footer, size = "md", side }: ModalProps) {
  const ref = useRef<HTMLDialogElement>(null);
  const titleId = useId();
  useEffect(() => {
    const d = ref.current;
    if (!d) return;
    if (open && !d.open) d.showModal();
    if (!open && d.open) d.close();
  }, [open]);
  const width = { sm: "max-w-sm", md: "max-w-lg", lg: "max-w-3xl", xl: "max-w-6xl" }[size];
  return (
    <dialog
      ref={ref}
      aria-labelledby={titleId}
      onClose={onClose}
      onKeyDown={(e) => {
        if (e.key === "Escape") onClose();
      }}
      onClick={(e) => {
        if (e.target === ref.current) onClose();
      }}
      className={cn(
        "m-auto w-full rounded-(--radius-dialog) border border-border bg-surface p-0 text-text shadow-(--shadow-pop) backdrop:bg-(--backdrop)",
        side && "dialog-side ml-auto mr-0 mt-0 mb-0 h-full max-h-full w-full max-w-xl rounded-none",
        !side && width,
      )}
    >
      {open && (
        <div className={cn("flex max-h-[90vh] flex-col", side && "h-full max-h-full")}>
          <div className="flex items-center justify-between border-b border-border px-5 py-3">
            <h2 id={titleId} className="font-semibold">
              {title}
            </h2>
            <button
              type="button"
              aria-label="Close"
              onClick={onClose}
              className="flex size-7 items-center justify-center rounded-full text-lg leading-none text-muted hover:rotate-90 hover:bg-surface-2 hover:text-text"
            >
              &times;
            </button>
          </div>
          <div className="min-h-0 flex-1 overflow-auto px-5 py-4">{children}</div>
          {footer && (
            <div className="flex justify-end gap-2 border-t border-border px-5 py-3">{footer}</div>
          )}
        </div>
      )}
    </dialog>
  );
}

/** Ask before a hard-to-undo action. `typed` makes the person type a word first. */
export function ConfirmDialog({
  open,
  onClose,
  title,
  text,
  confirmLabel,
  typed,
  danger = true,
  onConfirm,
}: {
  open: boolean;
  onClose: () => void;
  title: string;
  text: ReactNode;
  confirmLabel: string;
  typed?: string;
  danger?: boolean;
  onConfirm: () => Promise<unknown> | unknown;
}) {
  const [word, setWord] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  useEffect(() => {
    if (open) {
      setWord("");
      setError(null);
    }
  }, [open]);
  const ok = !typed || word === typed;
  return (
    <Modal
      open={open}
      onClose={onClose}
      title={title}
      size="sm"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button
            variant={danger ? "danger" : "primary"}
            disabled={!ok}
            busy={busy}
            onClick={async () => {
              setBusy(true);
              setError(null);
              try {
                await onConfirm();
                onClose();
              } catch (e) {
                setError(e);
              } finally {
                setBusy(false);
              }
            }}
          >
            {confirmLabel}
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        <div>{text}</div>
        {typed && (
          <label className="flex flex-col gap-1.5 text-sm">
            <span>
              Type <b>{typed}</b> to confirm
            </span>
            <input
              value={word}
              onChange={(e) => setWord(e.target.value)}
              className={cn(CONTROL, "h-9")}
              autoComplete="off"
            />
          </label>
        )}
        {error != null && <ErrorNote error={error} />}
      </div>
    </Modal>
  );
}

/* ---------- tabs ---------- */

export function Tabs<T extends string>({
  items,
  value,
  onChange,
  label,
}: {
  items: { value: T; label: ReactNode }[];
  value: T;
  onChange: (v: T) => void;
  label: string;
}) {
  return (
    <div role="tablist" aria-label={label} className="mb-4 flex gap-1 border-b border-border">
      {items.map((it) => (
        <button
          key={it.value}
          type="button"
          role="tab"
          aria-selected={it.value === value}
          onClick={() => onChange(it.value)}
          className={cn(
            "-mb-px border-b-2 px-3 py-2 text-sm duration-200",
            it.value === value
              ? "border-(--tab-indicator) font-medium"
              : "border-transparent text-muted hover:text-text",
          )}
        >
          {it.label}
        </button>
      ))}
    </div>
  );
}

/** Tabs that are pages of their own (the address changes). */
export function LinkTabs({
  items,
  label,
}: {
  items: { to: string; params?: Record<string, string>; label: string }[];
  label: string;
}) {
  return (
    <nav aria-label={label} className="mb-4 flex gap-1 overflow-x-auto border-b border-border">
      {items.map((it) => (
        <Link
          key={it.to + it.label}
          // biome-ignore lint/suspicious/noExplicitAny: the tab list is built from route names
          to={it.to as any}
          // biome-ignore lint/suspicious/noExplicitAny: same
          params={it.params as any}
          className="-mb-px whitespace-nowrap border-b-2 border-transparent px-3 py-2 text-sm text-muted duration-200 hover:border-border hover:text-text"
          activeProps={{ className: "border-(--tab-indicator) font-medium text-text" }}
        >
          {it.label}
        </Link>
      ))}
    </nav>
  );
}

/* ---------- tables ---------- */

export const TH = "ui-th px-3 py-2 text-left text-(length:--th-size) leading-4 font-medium text-muted";
export const TD = "px-3 py-2 align-middle";

export function Table({ children, className }: { children: ReactNode; className?: string }) {
  return (
    <div
      className={cn(
        "animate-rise-in overflow-x-auto rounded-(--radius-card) border border-border bg-surface shadow-(--shadow-card)",
        className,
      )}
    >
      <table className="w-full border-collapse text-[13px]">{children}</table>
    </div>
  );
}

export function Head({ cols }: { cols: (string | ReactNode)[] }) {
  return (
    <thead className="bg-(--table-head-bg)">
      <tr>
        {cols.map((c, i) => (
          <th key={i} scope="col" className={TH}>
            {c}
          </th>
        ))}
      </tr>
    </thead>
  );
}

export function useDebounced<T>(value: T, ms: number): T {
  const [v, setV] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setV(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return v;
}
