import type { QueryClient } from "@tanstack/react-query";
import { createRootRouteWithContext, Outlet } from "@tanstack/react-router";
import { Toaster } from "sonner";
import { useResolvedTheme } from "@/theme/theme";

interface RouterContext {
  queryClient: QueryClient;
}

export const Route = createRootRouteWithContext<RouterContext>()({
  component: Root,
  notFoundComponent: () => (
    <div className="flex h-full flex-col items-center justify-center gap-2 text-center">
      <h1 className="text-lg font-semibold">Page not found</h1>
      <a href="/" className="text-accent underline">
        Go to the start
      </a>
    </div>
  ),
});

function Root() {
  const theme = useResolvedTheme();
  return (
    <>
      <a
        href="#content"
        className="sr-only focus:not-sr-only focus:fixed focus:left-2 focus:top-2 focus:z-50 focus:rounded focus:bg-surface focus:px-3 focus:py-2"
      >
        Skip to the page
      </a>
      <Outlet />
      <Toaster theme={theme} position="bottom-right" />
    </>
  );
}
