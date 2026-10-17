import createClient, { type Middleware } from "openapi-fetch";
import { problemFrom } from "./errors";
import type { paths } from "./schema";

/**
 * The one way to call the API. Same origin, session cookie, and the CSRF token on every change.
 * The CSRF token lives in memory only (never in storage or in a URL).
 */
let csrfToken: string | null = null;
export function setCsrfToken(token: string | null): void {
  csrfToken = token;
}

let onUnauthorized: () => void = () => {};
/** Called when a call that needs a login gets `401` (the session ended). */
export function setUnauthorizedHandler(handler: () => void): void {
  onUnauthorized = handler;
}

let refreshCsrf: () => Promise<void> = async () => {};
/** Called once when the server says the CSRF token is not good; it should load a new one. */
export function setCsrfRefresher(refresh: () => Promise<void>): void {
  refreshCsrf = refresh;
}

const SAFE = new Set(["GET", "HEAD", "OPTIONS"]);
const copies = new WeakMap<Request, Request>();

/** The login routes answer 401 for a wrong password. That is not a lost session. */
function isAuthCall(url: string): boolean {
  return new URL(url).pathname.startsWith("/v1/auth/");
}

const middleware: Middleware = {
  async onRequest({ request }) {
    if (!SAFE.has(request.method) && csrfToken) request.headers.set("X-CSRF-Token", csrfToken);
    // A request body can be read once. Keep a copy for the one retry.
    if (!SAFE.has(request.method)) copies.set(request, request.clone());
    return request;
  },
  async onResponse({ request, response }) {
    if (response.status === 401 && !isAuthCall(request.url)) onUnauthorized();
    if (response.status === 403 && !SAFE.has(request.method)) {
      const body = await response
        .clone()
        .json()
        .catch(() => undefined);
      if (problemFrom(response, body).code === "csrf_failed") {
        await refreshCsrf();
        const again = copies.get(request);
        if (again && csrfToken) {
          again.headers.set("X-CSRF-Token", csrfToken);
          return fetch(again);
        }
      }
    }
    return undefined;
  },
};

export const api = createClient<paths>({ baseUrl: "", credentials: "same-origin" });
api.use(middleware);

/** The data of a call, or an `ApiProblem` thrown. */
export async function unwrap<T>(
  call: Promise<{ data?: T; error?: unknown; response: Response }>,
): Promise<T> {
  const { data, error, response } = await call;
  if (!response.ok) throw problemFrom(response, error);
  return data as T;
}

/** Same, for answers with no body (204). */
export async function unwrapEmpty(
  call: Promise<{ error?: unknown; response: Response }>,
): Promise<void> {
  const { error, response } = await call;
  if (!response.ok) throw problemFrom(response, error);
}
