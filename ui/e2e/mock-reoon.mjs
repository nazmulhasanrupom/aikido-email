// A tiny fake Reoon for the browser tests. Key "good-key" works. An address that starts with "bad" is invalid.
import { createServer } from "node:http";

createServer((req, res) => {
  const url = new URL(req.url, "http://x");
  res.setHeader("content-type", "application/json");
  if (url.searchParams.get("key") !== "good-key") {
    res.statusCode = 401;
    return res.end("{}");
  }
  if (url.pathname.includes("check-account-balance")) {
    return res.end(JSON.stringify({ api_status: "success", remaining_daily_credits: 150, remaining_instant_credits: 5000, status: "ok" }));
  }
  const email = url.searchParams.get("email") ?? "";
  const bad = email.startsWith("bad");
  res.end(JSON.stringify({ status: bad ? "invalid" : "safe", overall_score: bad ? 5 : 98, is_safe_to_send: !bad }));
}).listen(Number(process.argv[2] ?? 18091), "127.0.0.1");
