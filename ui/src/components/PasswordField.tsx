import { Eye, EyeOff } from "lucide-react";
import { type ComponentProps, forwardRef, useState } from "react";
import { Field } from "./Field";

type Props = Omit<ComponentProps<typeof Field>, "type" | "adornment">;

/** A password input with a show/hide button. */
export const PasswordField = forwardRef<HTMLInputElement, Props>(
  function PasswordField(props, ref) {
    const [shown, setShown] = useState(false);
    return (
      <Field
        ref={ref}
        type={shown ? "text" : "password"}
        adornment={
          <button
            type="button"
            onClick={() => setShown((s) => !s)}
            aria-label={shown ? "Hide password" : "Show password"}
            aria-pressed={shown}
            className="rounded p-1.5 text-muted hover:bg-text/5 hover:text-text"
          >
            {shown ? <EyeOff size={16} /> : <Eye size={16} />}
          </button>
        }
        {...props}
      />
    );
  },
);
