import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";
import "./styles.css";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { createRouter, RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { fetchAuthState } from "./api/auth";
import { setCsrfRefresher, setUnauthorizedHandler } from "./api/client";
import { ApiProblem } from "./api/errors";
import { routeTree } from "./routeTree.gen";
import { watchSystemTheme } from "./theme/theme";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // A 4xx answer will not get better by asking again.
      retry: (count, error) => !(error instanceof ApiProblem && error.status < 500) && count < 2,
      refetchOnWindowFocus: false,
      refetchIntervalInBackground: false,
    },
  },
});

const router = createRouter({
  routeTree,
  context: { queryClient },
  defaultPreload: "intent",
  // The page area cross-fades on a page change (see #content in styles.css).
  defaultViewTransition: true,
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}

// The session ended: drop everything we know and go to the login page, then back here.
setUnauthorizedHandler(() => {
  queryClient.clear();
  const here = window.location.pathname + window.location.search;
  void router.navigate({ to: "/login", search: { next: here } });
});
setCsrfRefresher(async () => {
  await queryClient.fetchQuery({
    queryKey: ["auth", "state"],
    queryFn: fetchAuthState,
    staleTime: 0,
  });
});

watchSystemTheme();

const root = document.getElementById("root");
if (!root) throw new Error("no #root element");
createRoot(root).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
);
