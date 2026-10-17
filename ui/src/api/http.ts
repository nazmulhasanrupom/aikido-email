import { api, unwrap, unwrapEmpty } from "./client";
import type { components } from "./schema";

/** Short names for the API types we use a lot. */
export type S = components["schemas"];

type Params = {
  path?: Record<string, string | number>;
  query?: Record<string, unknown>;
};
type Init = Params & { body?: unknown };
type Raw = Promise<{ data?: unknown; error?: unknown; response: Response }>;
type Loose = Record<
  "GET" | "POST" | "PUT" | "PATCH" | "DELETE",
  (url: string, init?: { params?: Params; body?: unknown; bodySerializer?: unknown }) => Raw
>;

// The generated client checks every path and body. For the many small calls of the pages,
// one loose door is easier: the data types still come from the generated schema.
const loose = api as unknown as Loose;

function clean(query: Record<string, unknown> | undefined): Record<string, unknown> | undefined {
  if (!query) return undefined;
  const out: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(query)) {
    if (v !== undefined && v !== null && v !== "") out[k] = v;
  }
  return out;
}

function params(init: Init | undefined): Params | undefined {
  if (!init) return undefined;
  const query = clean(init.query);
  return { ...(init.path ? { path: init.path } : {}), ...(query ? { query } : {}) };
}

export const http = {
  get: <T>(url: string, init?: Params) =>
    unwrap<T>(loose.GET(url, { params: params(init) }) as never),
  post: <T>(url: string, init?: Init) =>
    unwrap<T>(loose.POST(url, { params: params(init), body: init?.body }) as never),
  put: <T>(url: string, init?: Init) =>
    unwrap<T>(loose.PUT(url, { params: params(init), body: init?.body }) as never),
  patch: <T>(url: string, init?: Init) =>
    unwrap<T>(loose.PATCH(url, { params: params(init), body: init?.body }) as never),
  del: <T = void>(url: string, init?: Init) =>
    unwrap<T>(loose.DELETE(url, { params: params(init), body: init?.body }) as never),
  /** For answers with no body. */
  postEmpty: (url: string, init?: Init) =>
    unwrapEmpty(loose.POST(url, { params: params(init), body: init?.body }) as never),
  delEmpty: (url: string, init?: Init) =>
    unwrapEmpty(loose.DELETE(url, { params: params(init), body: init?.body }) as never),
};

/** A multipart upload. The CSRF header is added by the shared client middleware. */
export async function upload<T>(
  url: string,
  form: FormData,
  query?: Record<string, unknown>,
): Promise<T> {
  return unwrap<T>(
    api.POST(
      url as never,
      {
        params: { query: clean(query) } as never,
        body: form as never,
        bodySerializer: (b: unknown) => b as BodyInit,
      } as never,
    ) as never,
  );
}
