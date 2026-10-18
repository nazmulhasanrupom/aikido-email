/** Plain words for the codes of the API (section 12 of the UI build map). Extend as pages need them. */
const WORDS: Record<string, Record<string, string>> = {
  account: {
    active: "Active",
    paused: "Paused",
    error: "Stopped (problem)",
    disconnected: "Needs reconnect",
  },
  read: {
    reconnect_needed: "Reconnect to see replies",
    login_failed: "Inbox login failed",
    not_configured: "Reply reading not set up",
  },
  wait: {
    not_send_day: "Not a sending day",
    blocked_date: "Day off",
    outside_window: "Outside sending hours",
    daily_limit_reached: "Daily limit reached",
    hourly_limit_reached: "Hourly limit reached",
    gap_not_elapsed: "Waiting between emails",
    reply_check_stale: "Checking the inbox for replies first",
    reply_check_unavailable: "Cannot read the inbox, follow-ups wait",
    email_not_ready: "Waiting for the email to be written",
    email_review: "Waiting for you to approve the email",
  },
  job: {
    queued: "Queued",
    sending: "Sending",
    sent: "Sent",
    failed: "Failed",
    cancelled: "Cancelled",
    skipped: "Skipped (blocked address)",
    stuck: "Unknown result (may have been sent)",
  },
  enrollment: {
    active: "Active",
    waiting_send: "In the send queue",
    paused: "Paused",
    finished: "Finished",
    stopped: "Stopped",
    failed: "Failed",
  },
  stop: {
    reply: "Replied",
    company_replied: "A colleague replied",
    auto_reply: "Out of office",
    manual: "Stopped by you",
    stalled: "Waited too long",
    unsubscribe: "Unsubscribed",
    bounce: "Bounced",
    complaint: "Spam complaint",
  },
  skip: {
    suppressed: "Blocked",
    unverified: "Email not checked",
    active_elsewhere: "In another campaign",
    missing_variable: "Missing a value",
    no_healthy_account: "No healthy account",
    company_replied: "A colleague replied",
    campaign_archived: "Campaign archived",
    already_in_campaign: "Already in the campaign",
    lead_deleted: "Lead deleted",
  },
  label: {
    reply: "Reply",
    auto_reply: "Out of office",
    unsubscribe: "Unsubscribe",
    bounce_hard: "Bounced (bad address)",
    bounce_soft: "Bounced (temporary)",
    bounce_block: "Blocked by server",
    complaint: "Spam complaint",
    delayed: "Delayed",
    unknown: "Unknown",
  },
  suppression: {
    replied: "Replied",
    hard_bounce: "Bad address",
    unsubscribed: "Unsubscribed",
    complained: "Spam complaint",
    manual: "Added by you",
    erased: "Lead deleted",
  },
  verification: {
    mx_ok: "Checked",
    syntax_ok: "Looks right",
    no_mx: "Domain has no mail server",
    invalid: "Not valid",
    unknown: "Not checked",
  },
  contact: {
    new: "New",
    in_sequence: "In a sequence",
    contacted: "Contacted",
    replied: "Replied",
    bounced: "Bounced",
    unsubscribed: "Unsubscribed",
    completed: "Finished, no reply",
  },
  interest: {
    interested: "Interested",
    meeting_booked: "Meeting booked",
    won: "Won",
    not_interested: "Not interested",
    wrong_person: "Wrong person",
    lost: "Lost",
  },
  campaign: {
    draft: "Draft",
    active: "Active",
    paused: "Paused",
    completed: "Completed",
    archived: "Archived",
  },
  campaignLead: { pending: "Pending", enrolled: "In sequence", skipped: "Skipped" },
  gen: {
    pending: "Waiting",
    pending_review: "Needs review",
    approved: "Approved",
    ready: "Ready",
    rejected: "Written again",
    failed: "Failed",
    stale: "Old",
    cancelled: "Cancelled",
    used: "Sent",
  },
  importStatus: {
    uploaded: "Waiting for your mapping",
    pending: "Waiting",
    running: "Running",
    done: "Done",
    failed: "Failed",
    cancelled: "Cancelled",
  },
};

export type WordGroup = keyof typeof WORDS;

/** The words for a code. An unknown code shows itself with spaces. */
export function say(group: WordGroup, code: string | null | undefined): string {
  if (!code) return "";
  return WORDS[group]?.[code] ?? code.replaceAll("_", " ");
}

export const INTERESTS = Object.keys(WORDS.interest ?? {});
export const CONTACT_STATES = Object.keys(WORDS.contact ?? {});
export const LABELS = Object.keys(WORDS.label ?? {});

/** Errors of launch checks and other 422 codes that need more words than the server message. */
export const PROBLEM_WORDS: Record<string, string> = {
  no_steps: "The sequence has no emails yet.",
  no_accounts: "Choose at least one email account for this campaign.",
  no_healthy_account: "None of the chosen accounts can send right now.",
  generator_not_ready: "Set up an AI model first.",
  smtp_login_failed:
    "We could not log in to the SMTP server. Check the host, port and password. Many providers need an app password.",
  imap_login_failed:
    "We could not log in to the IMAP server. Check the host, port and password. Many providers need an app password.",
  oauth_not_configured: "Google (or Microsoft) login is not set up on this server yet.",
  step_in_use: "This step has emails waiting in the queue. Stop them first or keep the step.",
  provider_in_use: "An AI setting still uses this model. Choose another model there first.",
  last_admin_key: "This is the last admin key. Make another one first.",
};
