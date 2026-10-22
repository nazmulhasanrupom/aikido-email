/**
 * The text of the Guide, as data. Edit the words here. Write `code` between backticks.
 * `live` puts a value of this server in the section (with a Copy button).
 */
export interface GuideSection {
  id: string;
  title: string;
  /** What it is for. */
  about: string;
  /** What you need before you start. */
  need: string[];
  steps: string[];
  /** Common errors: what you see, and what to do. */
  errors: { problem: string; fix: string }[];
  /** A link inside the app. */
  link?: { label: string; to: "environment" | "accounts" | "integrations" | "api" | "leads" };
  live?: "redirect-google" | "redirect-microsoft" | "public-url";
}

export const SECTIONS: GuideSection[] = [
  {
    id: "start",
    title: "Start here",
    about: "The order in which to set things up. Each step has its own part in this guide.",
    need: ["The owner account (you made it already)."],
    steps: [
      "Connect an email account. This is the inbox your emails are sent from (parts: Gmail, Outlook, IMAP).",
      "Add an AI model. This is optional. It writes the personal parts of your emails.",
      "Import your leads. Open Leads and drop a CSV file. The columns are matched for you.",
      "Make a campaign. Pick a sequence, the sender inboxes and a daily limit.",
      "Tip: turn on `AIKIDO_DRY_RUN=true` in `.env` for the first run. The server then logs emails and sends nothing.",
    ],
    errors: [
      {
        problem: "I do not know where to begin.",
        fix: "Open Email Accounts and connect one inbox. The rest can wait.",
      },
    ],
    link: { label: "Open Email Accounts", to: "accounts" },
  },
  {
    id: "google",
    title: "Gmail (Google OAuth)",
    about: "Lets you connect a Gmail or Google Workspace inbox with one click.",
    need: [
      "A Google account.",
      "The public address of this server (see the part Public address and HTTPS).",
    ],
    steps: [
      "Open the Google Cloud Console and make a new project.",
      "Open APIs & Services → Library. Find Gmail API and press Enable.",
      "Open OAuth consent screen. Pick Internal if you use Google Workspace. If not, pick External, keep it in Testing mode and add your own address as a test user.",
      "Add the scopes `gmail.readonly` and `gmail.send`.",
      "Open Credentials → Create credentials → OAuth client ID. Pick the type Web application.",
      "Under Authorized redirect URIs, add the redirect address shown below. Copy it with the button.",
      "Copy the Client ID and the Client secret.",
      "Open Settings → Environment. Paste both values in the Google card and press Save.",
      "Press Test to check the shape of the keys.",
      "Open Email Accounts → Connect account → Google.",
    ],
    errors: [
      {
        problem: "Connect Gmail is grey.",
        fix: "The Client ID or the secret is not saved yet. Save both in Settings → Environment.",
      },
      {
        problem: "Google says redirect_uri_mismatch.",
        fix: "The redirect address in Google must match the one below, letter for letter. Check http or https and the port.",
      },
      {
        problem: "The account goes to Needs reconnect after 7 days.",
        fix: "An External app in Testing mode loses its tokens after 7 days. Use Internal (Workspace), or publish and verify the app.",
      },
      {
        problem: "Google asks for a security review.",
        fix: "`gmail.readonly` is a restricted scope. A public app needs a Google review. A private Internal app does not.",
      },
    ],
    link: { label: "Open Settings → Environment", to: "environment" },
    live: "redirect-google",
  },
  {
    id: "microsoft",
    title: "Outlook / Microsoft 365",
    about: "Lets you connect an Outlook or Microsoft 365 inbox with one click.",
    need: [
      "A Microsoft account that can open the Entra admin center.",
      "The public address of this server.",
    ],
    steps: [
      "Open the Microsoft Entra admin center. Go to App registrations → New registration.",
      "Pick the account types you want. Use Any organizational directory and personal accounts if you are not sure.",
      "Under Redirect URI, pick the type Web and paste the redirect address shown below.",
      "Open API permissions → Add a permission → Microsoft Graph → Delegated. Add `Mail.Read`, `Mail.Send`, `offline_access` and `User.Read`.",
      "If your organization needs it, press Grant admin consent.",
      "Open Certificates & secrets → New client secret. Copy the Value of the secret. Do not copy the Secret ID.",
      "Open Overview and copy the Application (client) ID.",
      "Pick the tenant: `common` for any account, or your own tenant ID for one organization.",
      "Open Settings → Environment. Paste the values in the Microsoft card and press Save.",
      "Press Test, then open Email Accounts → Connect account → Microsoft.",
    ],
    errors: [
      {
        problem: "The secret does not work.",
        fix: "You may have copied the Secret ID. Make a new secret and copy its Value at once. It is shown one time.",
      },
      {
        problem: "Microsoft says the redirect address does not match.",
        fix: "Add the exact address below as a Web redirect URI.",
      },
      {
        problem: "Microsoft needs admin approval.",
        fix: "Ask an admin of the tenant to press Grant admin consent in API permissions.",
      },
    ],
    link: { label: "Open Settings → Environment", to: "environment" },
    live: "redirect-microsoft",
  },
  {
    id: "imap",
    title: "IMAP / SMTP (any provider)",
    about: "Connects any mailbox with a host, a port and a login. No keys are needed.",
    need: [
      "The SMTP host and port (to send).",
      "The IMAP host and port (to read replies).",
      "The login. Many providers want an app password.",
    ],
    steps: [
      "Open Email Accounts → Connect account → Other.",
      "Gmail: SMTP `smtp.gmail.com` port 465, IMAP `imap.gmail.com` port 993. Turn on 2-step verification, then make an app password in your Google account (Security → App passwords).",
      "Outlook.com: SMTP `smtp-mail.outlook.com` port 587 (STARTTLS), IMAP `outlook.office365.com` port 993. Use an app password if you have 2-step verification.",
      "Zoho: SMTP `smtp.zoho.com` port 465, IMAP `imap.zoho.com` port 993.",
      "Yahoo: SMTP `smtp.mail.yahoo.com` port 465, IMAP `imap.mail.yahoo.com` port 993. Make an app password in Account security.",
      "Your own server: use the host names and ports of your provider.",
      "Press Test and save. The password is encrypted before it is saved.",
    ],
    errors: [
      {
        problem: "We could not log in to the IMAP server.",
        fix: "Check the host, the port and the password. Use an app password if the provider has 2-step verification.",
      },
      {
        problem: "We can send but cannot see replies.",
        fix: "The IMAP part failed. Open the account → Reply check and fix the IMAP login.",
      },
    ],
    link: { label: "Open Email Accounts", to: "accounts" },
  },
  {
    id: "ai",
    title: "AI model",
    about:
      "An AI writes the personal parts of your emails and matches the columns of your CSV files. It is optional.",
    need: ["An API key from a provider, or a local model."],
    steps: [
      "Anthropic: open the Anthropic console → API keys → Create key. Copy it.",
      "OpenAI: open the OpenAI platform → API keys → Create new secret key.",
      "OpenRouter: open openrouter.ai → Keys → Create key. Use the type OpenAI-compatible with the base URL `https://openrouter.ai/api/v1`.",
      "Ollama (a model on your own computer): install Ollama, pull a model, and use the type OpenAI-compatible with the base URL of Ollama, for example `http://host.docker.internal:11434/v1` from Docker. No key is needed.",
      "Open Settings → Integrations (or the AI screen). Add the model, paste the key and press Test.",
    ],
    errors: [
      {
        problem: "The test fails.",
        fix: "Check the key, the model name and the base URL. For Ollama, check that the model is running.",
      },
      {
        problem: "Is my lead data private?",
        fix: "Lead data goes to the provider you pick: name, job title, company, website, location and notes. The email address, the phone number and the reply text are never sent. For zero third parties, use a local model.",
      },
    ],
    link: { label: "Open Settings → Integrations", to: "integrations" },
  },
  {
    id: "reoon",
    title: "Reoon email verifier",
    about: "Checks if a lead's address is real before you send. It is optional.",
    need: ["A Reoon account and its API key."],
    steps: [
      "Open Reoon, go to the API page and copy your key.",
      "Open Settings → Integrations and open the Reoon app.",
      "Paste the key and press Install. The key is encrypted.",
      "Pick if the check runs on import or by hand.",
    ],
    errors: [
      {
        problem: "Reoon did not accept this key.",
        fix: "Copy the key again from your Reoon API page.",
      },
    ],
    link: { label: "Open Settings → Integrations", to: "integrations" },
  },
  {
    id: "api",
    title: "Master API key",
    about: "One key that lets your own scripts call every action of the API.",
    need: ["An owner or admin login."],
    steps: [
      "Open Settings → API.",
      "Press Generate master key.",
      "Copy the key at once. It is shown one time and cannot be shown again.",
      "Send it in every call: `Authorization: Bearer <key>`.",
      "Section keys are optional. A section key opens only one part of the API.",
      "Open `/docs` on this server for the full list of calls.",
    ],
    errors: [
      {
        problem: "I lost the key.",
        fix: "Rotate it in Settings → API, or make a new one. Only a hash is stored.",
      },
      {
        problem: "403 wrong_scope.",
        fix: "A section key was used on another part. Use the master key.",
      },
    ],
    link: { label: "Open Settings → API", to: "api" },
  },
  {
    id: "url",
    title: "Public address and HTTPS",
    about:
      "`AIKIDO_PUBLIC_URL` is the address you type in the browser. Google and Microsoft send people back to it after login, so they need it.",
    need: ["Access to the `.env` file on the server."],
    steps: [
      "Find the `.env` file. Linux and macOS: `~/aikido-email/.env`. Windows: `%USERPROFILE%\\aikido-email\\.env`.",
      "Set `AIKIDO_PUBLIC_URL=https://mail.example.com` (no slash at the end). On your own computer use `http://localhost:8080`.",
      "Restart: open a terminal in that folder and run `docker compose up -d`. On Windows use PowerShell: `cd $env:USERPROFILE\\aikido-email; docker compose up -d`.",
      "Login over plain HTTP works only on localhost, or with `AIKIDO_INSECURE_COOKIES=true` on a trusted network. For a real server use HTTPS.",
      "Caddy: put `mail.example.com { reverse_proxy 127.0.0.1:8080 }` in the Caddyfile. Caddy gets the certificate for you. Set `AIKIDO_TRUST_PROXY=true`.",
      "Coolify: open the app → Configuration → General → Domains, add `https://mail.example.com`, set the port to `8080`, save and redeploy.",
    ],
    errors: [
      {
        problem: "The web login fails.",
        fix: "`AIKIDO_PUBLIC_URL` must be the exact address in the browser. Use HTTPS, or localhost.",
      },
      { problem: "Login fails behind a proxy.", fix: "Set `AIKIDO_TRUST_PROXY=true`." },
    ],
    live: "public-url",
  },
  {
    id: "tracking",
    title: "Open tracking domain",
    about:
      "Counts opens with a normal image from your own domain, for example `media.acme.com`. It gets a free Let's Encrypt certificate.",
    need: ["A domain you control.", "The public address of this server."],
    steps: [
      "Open the tracking page of a campaign and turn tracking on. It shows one tracking domain for each sending domain.",
      "In your DNS provider, add the CNAME record it shows: type `CNAME`, name `media`, value = the host of this server. Use a CNAME only, no A record.",
      "If the domain is on Cloudflare, set the record to DNS only (grey cloud) until the certificate works.",
      "The server checks the record every 5 minutes for 72 hours, then gets the certificate.",
      "Coolify: open the app → Configuration → General → Domains. Add the tracking host after the ones you have, separated by a comma, for example `https://app.example.com,https://media.example.com`, with port `8080`. Save and redeploy.",
      "Check it: open `https://media.example.com/i/test.png`. A blank page without an error is right.",
    ],
    errors: [
      {
        problem: "The domain is green but no opens are counted.",
        fix: "Green only means the CNAME was found. The host must also open over HTTPS on your server.",
      },
    ],
  },
  {
    id: "backup",
    title: "Back up your key",
    about:
      "`AIKIDO_ENCRYPTION_KEY` locks every saved password, token and secret. Without it they cannot be read.",
    need: ["Access to the `.env` file on the server."],
    steps: [
      "Find the `.env` file. Linux and macOS: `~/aikido-email/.env`. Windows: `%USERPROFILE%\\aikido-email\\.env`.",
      "Copy the line `AIKIDO_ENCRYPTION_KEY=...` to a safe place, for example a password manager.",
      "If you run without that line, the server makes `encryption.key` in its data folder (`/data` in Docker). Back up that file instead.",
      "Also back up the database volume `pgdata`.",
    ],
    errors: [
      {
        problem: "Saved inboxes stopped working.",
        fix: "The key changed or was lost. Put the old key back, or connect the inboxes again. The keys saved in Settings → Environment must be typed again too.",
      },
    ],
  },
  {
    id: "trouble",
    title: "Problems",
    about: "The most common problems and their fix.",
    need: [],
    steps: [],
    errors: [
      {
        problem: "The server refuses to start.",
        fix: "`DATABASE_URL` is missing, or Postgres is down.",
      },
      { problem: "Connect Gmail is greyed out.", fix: "Add the keys in Settings → Environment." },
      {
        problem: "Web login fails.",
        fix: "Set `AIKIDO_PUBLIC_URL` to the exact browser address. Use HTTPS or localhost.",
      },
      { problem: "Login fails behind a proxy.", fix: "Set `AIKIDO_TRUST_PROXY=true`." },
      {
        problem: "Saved inboxes stopped working.",
        fix: "The encryption key changed or was lost. Restore it or connect the inboxes again.",
      },
      {
        problem: "A Gmail account goes disconnected after 7 days.",
        fix: "The Google app is in Testing mode. Use Internal, or publish and verify the app.",
      },
      {
        problem: "An account is disconnected (login failed).",
        fix: "Open the account. Google and Microsoft: press Reconnect. IMAP/SMTP: press Edit login, type the new password, then Test and save.",
      },
      {
        problem: "Follow-ups do not go out.",
        fix: "The inbox was not read lately. Use an account that can read replies.",
      },
      {
        problem: "I lost the owner password.",
        fix: "Run `docker compose exec app aikido_email reset-password <email>` on the server. It prints a one-time link.",
      },
    ],
  },
];
