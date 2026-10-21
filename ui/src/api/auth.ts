import { type QueryClient, queryOptions } from "@tanstack/react-query";
import { api, setCsrfToken, unwrap, unwrapEmpty } from "./client";
import type { components } from "./schema";

export type AuthState = components["schemas"]["AuthStateReply"];
export type User = components["schemas"]["UserOut"];
export type LoginReply = components["schemas"]["LoginReply"];

export const authStateKey = ["auth", "state"] as const;

export async function fetchAuthState(): Promise<AuthState> {
  const state = await unwrap(api.GET("/v1/auth/state"));
  setCsrfToken(state.csrf_token ?? null);
  return state;
}

export const authStateQuery = queryOptions({
  queryKey: authStateKey,
  queryFn: fetchAuthState,
  staleTime: 30_000,
});

/** After a login, setup or logout: load the new state now. (Marking it old is not enough: a route guard reads the cache.) */
export function refreshAuthState(queryClient: QueryClient): Promise<AuthState> {
  return queryClient.fetchQuery({ ...authStateQuery, staleTime: 0 });
}

export async function login(email: string, password: string): Promise<LoginReply> {
  const reply = await unwrap(api.POST("/v1/auth/login", { body: { email, password } }));
  setCsrfToken(reply.csrf_token);
  return reply;
}

export async function setup(body: components["schemas"]["SetupBody"]): Promise<LoginReply> {
  const reply = await unwrap(api.POST("/v1/auth/setup", { body }));
  setCsrfToken(reply.csrf_token);
  return reply;
}

export async function logout(): Promise<void> {
  await unwrapEmpty(api.POST("/v1/auth/logout"));
  setCsrfToken(null);
}

export async function acceptInvite(token: string, name: string, password: string) {
  const reply = await unwrap(
    api.POST("/v1/auth/invites/{token}/accept", {
      params: { path: { token } },
      body: { name, password },
    }),
  );
  setCsrfToken(reply.csrf_token);
  return reply;
}

export async function resetPassword(token: string, newPassword: string) {
  const reply = await unwrap(
    api.POST("/v1/auth/reset/{token}", {
      params: { path: { token } },
      body: { new_password: newPassword },
    }),
  );
  setCsrfToken(reply.csrf_token);
  return reply;
}

/** Remember the theme on the account, so it follows the user to another browser. */
export async function saveThemePreference(theme: string): Promise<void> {
  // The schema calls `preferences` an empty object type; the server accepts any JSON object.
  const preferences = { theme } as unknown as Record<string, never>;
  await unwrap(api.PATCH("/v1/users/me", { body: { preferences } }));
}

/** Remember the design (Generic or Aikido) on the account, like the theme. */
export async function saveDesignPreference(design: string): Promise<void> {
  const preferences = { design } as unknown as Record<string, never>;
  await unwrap(api.PATCH("/v1/users/me", { body: { preferences } }));
}
