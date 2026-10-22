import type { ReactNode } from "react";
import { toast } from "sonner";
import { saveDesignPreference } from "@/api/auth";
import { messageOf } from "@/api/errors";
import { Enso } from "@/components/Enso";
import { cn } from "@/lib/cn";
import { applyDesign, type DesignChoice, useDesign } from "@/theme/theme";

/** The three designs with a small picture each. Picking one applies it at once and saves it in this browser and in the account. */
export function DesignPicker() {
  const design = useDesign();
  async function pickDesign(d: DesignChoice) {
    applyDesign(d);
    try {
      await saveDesignPreference(d);
    } catch (e) {
      toast.error(messageOf(e));
    }
  }
  return (
    <fieldset className="flex flex-col gap-2">
      <legend className="mb-2 text-sm font-medium">Design</legend>
      <div className="grid gap-3 sm:grid-cols-3">
        <DesignTile
          value="generic"
          picked={design === "generic"}
          onPick={pickDesign}
          title="Generic"
          text="The first look: warm and simple, square corners, a light menu on the left."
          preview={<GenericPreview />}
        />
        <DesignTile
          value="indigo"
          picked={design === "indigo"}
          onPick={pickDesign}
          title="Indigo"
          text="The same layout in ink and paper, with an indigo accent and an ink menu."
          preview={<IndigoPreview />}
        />
        <DesignTile
          value="aikido"
          picked={design === "aikido"}
          onPick={pickDesign}
          title="Aikido"
          text="A new layout for daily work: pages on top, a Home page, a side panel and a live status bar."
          preview={<AikidoPreview />}
        />
      </div>
    </fieldset>
  );
}

function DesignTile({
  value,
  picked,
  onPick,
  title,
  text,
  preview,
}: {
  value: DesignChoice;
  picked: boolean;
  onPick: (d: DesignChoice) => void;
  title: string;
  text: string;
  preview: ReactNode;
}) {
  return (
    <button
      type="button"
      aria-pressed={picked}
      onClick={() => onPick(value)}
      className={cn(
        "flex flex-col gap-2 rounded-(--radius-card) border p-2 text-left hover:bg-surface-2",
        picked ? "border-accent ring-2 ring-accent/30" : "border-border",
      )}
    >
      <span
        aria-hidden="true"
        className="relative flex h-28 overflow-hidden rounded-(--radius-control) border border-border"
      >
        {preview}
      </span>
      <span className="px-1">
        <span className="flex items-center justify-between font-medium">
          {title}
          {picked && <span className="text-xs font-normal text-accent">In use</span>}
        </span>
        <span className="text-xs text-muted">{text}</span>
      </span>
    </button>
  );
}

/*
 * Small pictures of the designs. The colors are each design's own, so a picture looks the same in every theme.
 */

/** A menu on the left and a card: Generic and Indigo share this layout. */
function RailPreview({
  c,
}: {
  c: {
    bg: string;
    rail: string;
    railLine: string;
    card: string;
    line: string;
    accent: string;
    mark: string;
    radius: number;
    button: number;
  };
}) {
  return (
    <span className="flex flex-1" style={{ background: c.bg }}>
      <span
        className="flex w-1/4 flex-col gap-1.5 p-2"
        style={{ background: c.rail, borderRight: `1px solid ${c.railLine}` }}
      >
        <span className="h-2 w-2/3 rounded-sm" style={{ background: c.mark }} />
        <span className="mt-1 h-1.5 w-full rounded-sm" style={{ background: c.railLine }} />
        <span className="h-1.5 w-4/5 rounded-sm" style={{ background: c.railLine }} />
        <span className="h-1.5 w-full rounded-sm" style={{ background: c.railLine }} />
      </span>
      <span className="flex flex-1 flex-col gap-2 p-2.5">
        <span className="h-2.5 w-1/2 rounded-sm" style={{ background: "#14161b", opacity: 0.75 }} />
        <span
          className="flex flex-1 flex-col gap-1.5 border p-2"
          style={{ background: c.card, borderColor: c.line, borderRadius: c.radius }}
        >
          <span className="h-1.5 w-3/4 rounded-sm" style={{ background: c.line }} />
          <span className="h-1.5 w-1/2 rounded-sm" style={{ background: c.line }} />
          <span
            className="mt-auto h-3.5 w-12"
            style={{ background: c.accent, borderRadius: c.button }}
          />
        </span>
      </span>
    </span>
  );
}

function GenericPreview() {
  return (
    <RailPreview
      c={{
        bg: "#f6f1e9",
        rail: "#fbf8f3",
        railLine: "#e3d9ca",
        card: "#fbf8f3",
        line: "#e3d9ca",
        accent: "#98502d",
        mark: "#98502d",
        radius: 4,
        button: 3,
      }}
    />
  );
}

function IndigoPreview() {
  return (
    <>
      <RailPreview
        c={{
          bg: "#f3f1ec",
          rail: "#14161b",
          railLine: "#3a3f4b",
          card: "#fcfbf8",
          line: "#d6d1c5",
          accent: "#2b3a78",
          mark: "#ff7a5c",
          radius: 10,
          button: 999,
        }}
      />
      <Enso size={54} className="absolute -right-3 -bottom-3 text-[#c73a28] opacity-80" />
    </>
  );
}

/** The Aikido layout: the dark top bar, a section menu, panels, the side panel and the status bar. */
function AikidoPreview() {
  const line = "#e2e5ea";
  return (
    <span className="flex flex-1 flex-col" style={{ background: "#f4f5f7" }}>
      <span
        className="flex h-4 shrink-0 items-center gap-1.5 px-1.5"
        style={{ background: "#0e0f13" }}
      >
        <span className="size-2 rounded-[2px]" style={{ background: "#d92d20" }} />
        <span className="relative h-1 w-4 rounded-sm" style={{ background: "#f2f3f5" }}>
          <span
            className="absolute inset-x-0 -bottom-[5px] h-0.5"
            style={{ background: "#ff4d3d" }}
          />
        </span>
        <span className="h-1 w-4 rounded-sm" style={{ background: "#585d68" }} />
        <span className="h-1 w-3 rounded-sm" style={{ background: "#585d68" }} />
        <span className="ml-auto h-2 w-5 rounded-[2px]" style={{ background: "#f2f3f5" }} />
      </span>
      <span className="flex min-h-0 flex-1 gap-1.5 p-1.5">
        <span className="flex w-1/6 flex-col gap-1">
          <span
            className="h-2 rounded-[2px] bg-white"
            style={{ boxShadow: `inset 2px 0 0 #d92d20, 0 0 0 1px ${line}` }}
          />
          <span className="h-1 w-4/5 rounded-sm" style={{ background: "#c9ced6" }} />
          <span className="h-1 w-3/5 rounded-sm" style={{ background: "#c9ced6" }} />
        </span>
        <span className="flex flex-1 flex-col gap-1.5">
          {[0, 1].map((i) => (
            <span
              key={i}
              className="flex flex-1 flex-col gap-1 rounded-[3px] border bg-white p-1.5"
              style={{ borderColor: line }}
            >
              <span className="flex items-center gap-1">
                <span className="h-1.5 w-[2px]" style={{ background: "#d92d20" }} />
                <span className="h-1 w-1/3 rounded-sm" style={{ background: "#9aa0ad" }} />
              </span>
              <span className="h-1 w-3/4 rounded-sm" style={{ background: line }} />
            </span>
          ))}
        </span>
        <span
          className="flex w-1/4 flex-col gap-1 rounded-[3px] border bg-white p-1.5"
          style={{ borderColor: line }}
        >
          <span
            className="h-1.5 w-2/3 rounded-sm"
            style={{ background: "#0e0f13", opacity: 0.8 }}
          />
          <span className="h-1 w-full rounded-sm" style={{ background: line }} />
          <span className="h-1 w-4/5 rounded-sm" style={{ background: line }} />
        </span>
      </span>
      <span
        className="flex h-2.5 shrink-0 items-center gap-1 border-t bg-white px-1.5"
        style={{ borderColor: line }}
      >
        <span className="size-1 rounded-full" style={{ background: "#17734a" }} />
        <span className="h-0.5 w-6 rounded-sm" style={{ background: "#9aa0ad" }} />
      </span>
    </span>
  );
}
