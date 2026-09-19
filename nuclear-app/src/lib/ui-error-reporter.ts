import { createErrorInbox, recordError } from './error-inbox';
import { normalizeAppError } from './frontend-errors';

export interface ErrorAttempt {
  fail: (error: unknown) => string;
}

export interface UiErrorReporter {
  begin: (source: string, context: string) => ErrorAttempt;
  resolve: (source: string) => void;
}

export class InterfaceErrorReporter implements UiErrorReporter {
  private nextAttempt = 1;

  constructor(
    private readonly inbox: ReturnType<typeof createErrorInbox>,
    private readonly settingsOpen: () => boolean,
    private readonly isActive: () => boolean
  ) {}

  begin(source: string, context: string): ErrorAttempt {
    const occurrence = `${source}:${this.nextAttempt++}`;
    this.resolve(source);
    return {
      fail: (error) => {
        const detail = normalizeAppError(error);
        if (this.isActive())
          recordError(
            this.inbox,
            { key: source, context, detail },
            occurrence,
            this.settingsOpen()
          );
        return detail;
      }
    };
  }

  resolve(source: string): void {
    if (this.isActive()) delete this.inbox.reportedActive[source];
  }
}
