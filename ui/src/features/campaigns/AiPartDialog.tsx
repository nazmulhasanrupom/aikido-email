import { useEffect, useState } from "react";
import type { S } from "@/api/http";
import { Button, Check2, Field, Modal, NumField, TextArea } from "@/components/ui";

const blank = (): S["AiSlot"] => ({
  key: "",
  instruction: "",
  max_words: 40,
  allow_links: false,
  fallback: "",
});

/** Define a part of the email that the AI writes for each lead. */
export function AiPartDialog({
  open,
  onClose,
  initial,
  takenKeys,
  onSave,
}: {
  open: boolean;
  onClose: () => void;
  initial: S["AiSlot"] | null;
  takenKeys: string[];
  onSave: (slot: S["AiSlot"], isNew: boolean) => void;
}) {
  const [s, setS] = useState<S["AiSlot"]>(blank());
  useEffect(() => {
    if (open) setS(initial ?? blank());
  }, [open, initial]);
  const badKey = !/^[A-Za-z_][A-Za-z0-9_]*$/.test(s.key);
  const taken = !initial && takenKeys.includes(s.key);
  return (
    <Modal
      open={open}
      onClose={onClose}
      title={initial ? "Edit AI part" : "New AI part"}
      size="md"
      footer={
        <>
          <Button variant="secondary" onClick={onClose}>
            Cancel
          </Button>
          <Button
            disabled={badKey || taken || !s.instruction.trim()}
            onClick={() => {
              onSave({ ...s, fallback: s.fallback || null }, !initial);
              onClose();
            }}
          >
            Save AI part
          </Button>
        </>
      }
    >
      <div className="flex flex-col gap-3">
        <Field
          label="Name"
          value={s.key}
          disabled={!!initial}
          onChange={(e) => setS({ ...s, key: e.target.value })}
          error={
            s.key && badKey
              ? "Use letters, digits and _, starting with a letter."
              : taken
                ? "You already have an AI part with this name."
                : undefined
          }
          help="Shown in the email as {{ai:name}}."
        />
        <TextArea
          label="What should the AI write?"
          value={s.instruction}
          onChange={(e) => setS({ ...s, instruction: e.target.value })}
          placeholder="One friendly line about their company."
        />
        <NumField
          label="Longest text"
          value={s.max_words ?? 40}
          min={1}
          max={400}
          suffix="words"
          onChange={(max_words) => setS({ ...s, max_words })}
        />
        <Check2
          label="Allow links"
          checked={!!s.allow_links}
          onChange={(allow_links) => setS({ ...s, allow_links })}
        />
        <TextArea
          label="Fallback text (recommended)"
          value={s.fallback ?? ""}
          onChange={(e) => setS({ ...s, fallback: e.target.value })}
          help="Used when the AI cannot write a good text. Your words, so you know what goes out."
        />
      </div>
    </Modal>
  );
}
