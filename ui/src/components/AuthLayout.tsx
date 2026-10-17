import type { ReactNode } from "react";
import { Card } from "./Card";
import { Logo } from "./Logo";

/** The centered card of the setup, login, invite and reset pages. */
export function AuthLayout({
  title,
  intro,
  children,
}: {
  title: string;
  intro?: string;
  children: ReactNode;
}) {
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
