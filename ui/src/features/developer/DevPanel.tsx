import { useCallback, useEffect, useRef, useState } from "react";
import { cn } from "@/lib/cn";
import { readStored, writeStored } from "@/lib/safe-storage";
import { LogsTab } from "./LogsTab";
import { SqlTab } from "./SqlTab";
import { ToolsTab } from "./ToolsTab";

const HEIGHT_KEY = "aikido.dev.height";
const TAB_KEY = "aikido.dev.tab";
const MIN = 120;
type Tab = "logs" | "sql" | "tools";

/** The developer panel at the bottom of the app. Drag the top edge to resize. */
export function DevPanel({ ws }: { ws: string }) {
  const [height, setHeight] = useState(() => {
    const n = Number(readStored(HEIGHT_KEY));
    return Number.isFinite(n) && n >= MIN ? n : 300;
  });
  const [tab, setTab] = useState<Tab>(() => (["logs", "sql", "tools"].includes(readStored(TAB_KEY) ?? "") ? (readStored(TAB_KEY) as Tab) : "logs"));
  const [minimized, setMinimized] = useState(false);
  const [errors, setErrors] = useState(0);
  const [warns, setWarns] = useState(0);
  const drag = useRef<{ y: number; h: number } | null>(null);

  const maxHeight = () => Math.max(MIN, Math.floor(window.innerHeight * 0.85));
  const onMove = useCallback((e: PointerEvent) => {
    if (!drag.current) return;
    setHeight(Math.min(maxHeight(), Math.max(MIN, drag.current.h + (drag.current.y - e.clientY))));
  }, []);
  const onUp = useCallback(() => {
    drag.current = null;
    window.removeEventListener("pointermove", onMove);
    window.removeEventListener("pointerup", onUp);
  }, [onMove]);
  useEffect(() => () => onUp(), [onUp]);
  useEffect(() => {
    writeStored(HEIGHT_KEY, String(height));
  }, [height]);

  const pick = (t: Tab) => {
    setTab(t);
    setMinimized(false);
    writeStored(TAB_KEY, t);
  };
  const counts = useCallback((e: number, w: number) => {
    setErrors(e);
    setWarns(w);
  }, []);

  return (
    <section
      aria-label="Developer panel"
      className="flex shrink-0 flex-col border-t border-border bg-surface"
      style={{ height: minimized ? undefined : height }}
    >
      {/* biome-ignore lint/a11y/useSemanticElements: a drag handle, with keys to resize */}
      <div
        role="separator"
        aria-orientation="horizontal"
        aria-label="Resize the developer panel"
        tabIndex={0}
        className="h-1.5 shrink-0 cursor-row-resize bg-border/60 hover:bg-accent"
        onPointerDown={(e) => {
          drag.current = { y: e.clientY, h: height };
          window.addEventListener("pointermove", onMove);
          window.addEventListener("pointerup", onUp);
        }}
        onKeyDown={(e) => {
          if (e.key === "ArrowUp") setHeight((h) => Math.min(maxHeight(), h + 30));
          if (e.key === "ArrowDown") setHeight((h) => Math.max(MIN, h - 30));
        }}
      />
      <div className="flex shrink-0 items-center gap-1 border-b border-border px-2 text-xs">
        <span className="mr-2 font-semibold">Developer</span>
        {(
          [
            ["logs", "Logs"],
            ["sql", "SQL"],
            ["tools", "Tools"],
          ] as [Tab, string][]
        ).map(([id, label]) => (
          <button
            key={id}
            type="button"
            className={cn("rounded-t px-2.5 py-1.5", tab === id && !minimized ? "bg-surface-2 font-medium" : "text-muted hover:text-text")}
            onClick={() => pick(id)}
          >
            {label}
            {id === "logs" && errors > 0 && <span className="ml-1.5 rounded-full bg-danger/20 px-1.5 text-danger">{errors}</span>}
            {id === "logs" && errors === 0 && warns > 0 && <span className="ml-1.5 rounded-full bg-warning/20 px-1.5 text-warning">{warns}</span>}
          </button>
        ))}
        <button type="button" className="ml-auto rounded px-2 py-1 text-muted hover:bg-surface-2" onClick={() => setMinimized(!minimized)}>
          {minimized ? "Open" : "Minimize"}
        </button>
      </div>
      {/* the logs keep polling while another tab is shown, so no line is missed */}
      <div className={cn("min-h-0 flex-1", minimized && "hidden")}>
        <div className={cn("h-full", tab !== "logs" && "hidden")}>
          <LogsTab onCounts={counts} />
        </div>
        <div className={cn("h-full", tab !== "sql" && "hidden")}>{tab === "sql" && <SqlTab />}</div>
        <div className={cn("h-full", tab !== "tools" && "hidden")}>
          <ToolsTab ws={ws} active={tab === "tools" && !minimized} />
        </div>
      </div>
    </section>
  );
}
