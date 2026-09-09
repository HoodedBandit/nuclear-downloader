export type PageDisposer = () => void;

export class PageLifetime {
  private active = true;
  private readonly disposers = new Set<PageDisposer>();

  constructor(private readonly reportDisposalError: (error: unknown) => void) {}

  get isActive(): boolean {
    return this.active;
  }

  own(disposer: PageDisposer): void {
    if (!this.active) {
      disposer();
      return;
    }
    this.disposers.add(disposer);
  }

  guard<TArguments extends unknown[]>(
    callback: (...arguments_: TArguments) => void
  ): (...arguments_: TArguments) => void {
    return (...arguments_: TArguments): void => {
      if (this.active) callback(...arguments_);
    };
  }

  dispose(): void {
    if (!this.active) return;
    this.active = false;
    const disposers = [...this.disposers];
    this.disposers.clear();
    for (const disposer of disposers) {
      try {
        disposer();
      } catch (error) {
        try {
          this.reportDisposalError(error);
        } catch {
          // Error reporting must not prevent the remaining resources from being released.
        }
      }
    }
  }
}
