/** An error answer of the API: `{ "error": { "code", "message" } }`. */
export class ApiProblem extends Error {
  readonly status: number;
  readonly code: string;
  readonly retryAfter: number | undefined;
  /** The whole `error` object of the answer (some codes carry a list such as `problems`). */
  readonly details: Record<string, unknown>;

  constructor(
    status: number,
    code: string,
    message: string,
    retryAfter?: number,
    details: Record<string, unknown> = {},
  ) {
    super(message);
    this.name = "ApiProblem";
    this.status = status;
    this.code = code;
    this.retryAfter = retryAfter;
    this.details = details;
  }
}

/** Plain words for the codes. A code that is not here shows the server's own message. */
const WORDS: Record<string, string> = {
  invalid_credentials: "Email or password is wrong.",
  too_many_attempts: "Too many tries. Wait a little, then try again.",
  setup_done: "The owner account exists already. Log in instead.",
  setup_code_wrong: "The setup code is wrong. Look in the server log, or in ./data/setup_code.txt.",
  password_too_short: "Use at least 12 characters.",
  password_too_long: "Use at most 128 characters.",
  password_too_weak: "This password is too easy to guess. Try a longer phrase of unrelated words.",
  password_has_personal_info: "Do not put your name or email in the password.",
  invalid_email: "This email address does not look right.",
  invalid_name: "Give a name (up to 100 characters).",
  invalid_workspace_name: "Give the workspace a name (up to 80 characters).",
  email_taken: "A user with this email exists already.",
  invite_invalid: "This invite link is not valid. It may be used or expired. Ask for a new one.",
  reset_invalid: "This reset link is not valid. It may be used or expired. Ask for a new one.",
  insecure_connection:
    "This address uses plain HTTP, so the browser would not keep your login. Use HTTPS.",
  csrf_failed: "The page is out of date. Reload it and try again.",
  admin_only: "Only an owner or an admin can do this.",
  wrong_password: "The current password is wrong.",
  not_logged_in: "Log in first.",
};

export function wordsFor(problem: ApiProblem): string {
  if (problem.code === "too_many_attempts" && problem.retryAfter) {
    const s = problem.retryAfter;
    const wait = s >= 90 ? `${Math.ceil(s / 60)} minutes` : s === 1 ? "1 second" : `${s} seconds`;
    return `Too many tries. Try again in ${wait}.`;
  }
  return WORDS[problem.code] ?? problem.message;
}

/** Text for any thrown value. */
export function messageOf(error: unknown): string {
  if (error instanceof ApiProblem) return wordsFor(error);
  if (error instanceof TypeError) return "The server did not answer. Check your connection.";
  return "Something went wrong.";
}

interface ErrorBody {
  error?: { code?: string; message?: string; [key: string]: unknown };
}

export function problemFrom(response: Response, body: unknown): ApiProblem {
  const e = (body as ErrorBody | undefined)?.error;
  const retry = Number(response.headers.get("retry-after"));
  return new ApiProblem(
    response.status,
    e?.code ?? "unknown_error",
    e?.message ?? `The server answered ${response.status}.`,
    Number.isFinite(retry) && retry > 0 ? retry : undefined,
    (e as Record<string, unknown> | undefined) ?? {},
  );
}
