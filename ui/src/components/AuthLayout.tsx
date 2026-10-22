import type { ReactNode } from "react";
import { useDesign } from "@/theme/theme";
import { Card } from "./Card";
import { Enso } from "./Enso";
import { Logo } from "./Logo";

/** The page of the setup, login, invite and reset forms. */
export function AuthLayout({
  title,
  intro,
  children,
}: {
  title: string;
  intro?: string;
  children: ReactNode;
}) {
  const design = useDesign();
  if (design === "aikido") {
    return (
      <AikidoAuth title={title} intro={intro}>
        {children}
      </AikidoAuth>
    );
  }
  if (design === "indigo") {
    return (
      <IndigoAuth title={title} intro={intro}>
        {children}
      </IndigoAuth>
    );
  }
  return (
    <main className="flex min-h-full items-center justify-center px-4 py-10">
      <div className="animate-fade-in w-full max-w-md">
        <div className="mb-6 flex items-center justify-center gap-3">
          <Logo size={32} />
          <span className="text-lg font-semibold tracking-tight">AIKIDO EMAIL</span>
        </div>
        <Card>
          <h1 className="text-xl font-semibold">{title}</h1>
          {intro && <p className="mt-1 text-muted">{intro}</p>}
          <div className="mt-5">{children}</div>
        </Card>
      </div>
    </main>
  );
}

/** The Indigo design: an ink panel with a brush circle on the left, the form on paper on the right. */
function IndigoAuth({ title, intro, children }: { title: string; intro?: string; children: ReactNode }) {
  return (
    <main className="grid min-h-full lg:grid-cols-[minmax(0,1.05fr)_minmax(0,1fr)]">
      <section
        aria-hidden="true"
        className="ui-sidebar relative hidden overflow-hidden bg-(--sidebar-bg) p-12 text-(--sidebar-fg) lg:flex lg:flex-col lg:justify-between"
      >
        <div className="relative flex items-center gap-3">
          <Logo size={34} />
          <span className="ui-label text-sm font-semibold">Aikido Email</span>
        </div>
        <Enso
          size={460}
          className="pointer-events-none absolute -right-28 top-1/2 -translate-y-1/2 text-(--sidebar-icon-active)"
        />
        <div className="relative max-w-sm">
          <p className="text-4xl font-semibold leading-tight tracking-tight">
            Cold email,
            <br />
            done with balance.
          </p>
          <ul className="mt-6 flex flex-col gap-2 text-sm text-(--sidebar-muted)">
            <li>Your own server. Your own data.</li>
            <li>Human timing on every send.</li>
            <li>A reply stops the sequence at once.</li>
          </ul>
        </div>
      </section>
      <section className="flex items-center justify-center px-6 py-12">
        <div className="animate-fade-in w-full max-w-sm">
          <div className="mb-10 flex items-center gap-3 lg:hidden">
            <Logo size={32} />
            <span className="ui-label text-sm font-semibold">Aikido Email</span>
          </div>
          <h1 className="text-(length:--page-title) leading-(--page-title-leading) font-semibold">
            {title}
          </h1>
          {intro && <p className="mt-1.5 text-muted">{intro}</p>}
          <div className="mt-7">{children}</div>
        </div>
      </section>
    </main>
  );
}

/** The Aikido design: the frame of the app (the dark top bar and the status line) around the form. */
function AikidoAuth({ title, intro, children }: { title: string; intro?: string; children: ReactNode }) {
  return (
    <div className="flex min-h-full flex-col">
      <header className="flex h-12 shrink-0 items-center gap-2 bg-(--topnav-bg) px-4 text-(--topnav-fg)">
        <Logo size={24} />
        <span className="text-[13px] font-semibold">Aikido Email</span>
      </header>
      <main className="flex flex-1 items-center justify-center px-4 py-10">
        <div className="animate-fade-in w-full max-w-sm rounded-(--radius-card) border border-border bg-surface p-6 shadow-(--shadow-card)">
          <h1 className="text-(length:--page-title) leading-(--page-title-leading) font-semibold">
            {title}
          </h1>
          {intro && <p className="mt-1 text-muted">{intro}</p>}
          <div className="mt-5">{children}</div>
        </div>
      </main>
      <footer className="flex h-7 shrink-0 items-center gap-4 border-t border-border bg-(--statusbar-bg) px-3 font-mono text-[11px] text-muted">
        <span className="flex items-center gap-1.5">
          <span aria-hidden="true" className="size-1.5 rounded-full bg-success" />
          Self-hosted
        </span>
        <span>Your server. Your data.</span>
      </footer>
    </div>
  );
}
