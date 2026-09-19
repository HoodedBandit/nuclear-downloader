export interface ErrorSource {
  key: string;
  context: string;
  detail: string | null | undefined;
}

export interface ErrorEntry {
  id: number;
  context: string;
  detail: string;
  occurredAt: number;
  read: boolean;
  occurrence?: string;
}

export function createErrorInbox() {
  return {
    entries: [] as ErrorEntry[],
    active: {} as Record<string, string>,
    reportedActive: {} as Record<string, string>,
    nextId: 1
  };
}

export function markErrorsRead(inbox: ReturnType<typeof createErrorInbox>): void {
  for (const entry of inbox.entries) entry.read = true;
}

export function recordError(
  inbox: ReturnType<typeof createErrorInbox>,
  source: ErrorSource,
  occurrence: string,
  settingsOpen: boolean,
  now = Date.now()
): void {
  if (!source.detail) return;
  inbox.reportedActive[source.key] = source.detail;
  if (inbox.entries.some((entry) => entry.occurrence === occurrence)) return;
  inbox.entries.unshift({
    id: inbox.nextId++,
    context: source.context,
    detail: source.detail,
    occurredAt: now,
    read: settingsOpen,
    occurrence
  });
  if (inbox.entries.length > 200) inbox.entries.splice(200);
}

// Keep resolved errors available for investigation without repeatedly notifying
// for an unchanged failure on every progress event.
export function syncErrors(
  inbox: ReturnType<typeof createErrorInbox>,
  sources: ErrorSource[],
  settingsOpen: boolean,
  now = Date.now()
): void {
  const active: Record<string, string> = {};
  for (const source of sources) {
    if (!source.detail) continue;
    active[source.key] = source.detail;
    if (inbox.active[source.key] !== source.detail) {
      inbox.entries.unshift({
        id: inbox.nextId++,
        context: source.context,
        detail: source.detail,
        occurredAt: now,
        read: settingsOpen
      });
    }
  }
  inbox.active = active;
  if (inbox.entries.length > 200) inbox.entries.splice(200);
  if (settingsOpen) markErrorsRead(inbox);
}
