export interface AccessibleDialogOptions {
  onClose: () => void;
  locked?: boolean;
}

const FOCUSABLE_SELECTOR = [
  'button:not([disabled])',
  '[href]',
  'input:not([disabled])',
  'select:not([disabled])',
  'textarea:not([disabled])',
  '[tabindex]:not([tabindex="-1"])'
].join(',');

export function accessibleDialog(
  node: HTMLElement,
  options: AccessibleDialogOptions
): { update: (next: AccessibleDialogOptions) => void; destroy: () => void } {
  let current = options;
  const previouslyFocused = document.activeElement as HTMLElement | null;
  const appRoot = document.querySelector<HTMLElement>('main');
  const wasInert = appRoot?.inert ?? false;
  if (appRoot) appRoot.inert = true;

  const focusInitial = (): void => {
    const requested = node.querySelector<HTMLElement>('[data-dialog-initial-focus]');
    const first = requested ?? node.querySelector<HTMLElement>(FOCUSABLE_SELECTOR);
    (first ?? node).focus();
  };

  const keydown = (event: KeyboardEvent): void => {
    if (event.key === 'Escape') {
      if (!current.locked) {
        event.preventDefault();
        current.onClose();
      }
      return;
    }

    if (event.key !== 'Tab') return;
    const focusable = Array.from(node.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
      (element) => !element.hidden && element.getAttribute('aria-hidden') !== 'true'
    );
    if (focusable.length === 0) {
      event.preventDefault();
      node.focus();
      return;
    }

    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  };

  node.addEventListener('keydown', keydown);
  queueMicrotask(focusInitial);

  return {
    update(next) {
      current = next;
    },
    destroy() {
      node.removeEventListener('keydown', keydown);
      if (appRoot) appRoot.inert = wasInert;
      previouslyFocused?.focus();
    }
  };
}
