export function normalizeAppError(error: unknown): string {
  if (error && typeof error === 'object') {
    const typed = error as {
      safe_summary?: unknown;
      safeSummary?: unknown;
      summary?: unknown;
      message?: unknown;
      code?: unknown;
    };
    const summary = typed.safe_summary ?? typed.safeSummary ?? typed.summary ?? typed.message;
    if (typeof summary === 'string' && summary.trim()) {
      const code = typeof typed.code === 'string' ? ` (${typed.code})` : '';
      return `${summary}${code}`;
    }
  }
  const message = String(error ?? 'Unknown error');
  return message.startsWith('Error: ') ? message.slice(7) : message;
}

export function appErrorDetail(error: unknown): string {
  if (error && typeof error === 'object') {
    try {
      return JSON.stringify(error);
    } catch {
      return normalizeAppError(error);
    }
  }
  return String(error ?? 'Unknown error');
}

export function normalizeDownloadError(message: string, code: string | null = null): string {
  if (code) return message;

  if (
    /(guest token|bad guest token|failed to query api|unauthorized)/i.test(message) &&
    /(twitter|x\.com|\[twitter\])/i.test(message)
  ) {
    return "X blocked anonymous access. Enable Cookies and make sure you're logged in, then retry.";
  }

  if (
    /saml|oauth|microsoftonline|okta|shibboleth|login required|authentication required|sign.?in to confirm|private video|members-only|age-restricted|confirm you'?re not a bot/i.test(
      message
    ) ||
    (/Unsupported URL/i.test(message) && /login|auth|sign.?in/i.test(message))
  ) {
    if (/confirm you'?re not a bot|not a bot/i.test(message)) {
      return 'YouTube requested bot verification for this public video. Update downloader runtime first, then retry.';
    }
    return "This site requires login. Enable Cookies (use Firefox or a cookies.txt file) and make sure you're logged in.";
  }

  if (
    /[Cc]ould not copy.*cookie|cookie.*database|cookies-from-browser|decrypt.*cookie|cookie.*locked/i.test(
      message
    )
  ) {
    return 'Browser cookie database is locked. Close your browser first, or switch to Firefox/cookie file mode.';
  }

  if (/cookie.*expired|cookies? are no longer valid|session expired/i.test(message)) {
    return 'Your login cookies were rejected. Refresh them from Firefox or export a new cookies.txt file and retry.';
  }

  return message;
}
