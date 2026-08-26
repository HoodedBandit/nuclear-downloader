// @vitest-environment jsdom

import { fireEvent, render, waitFor } from '@testing-library/svelte';
import { describe, expect, it } from 'vitest';
import AccessibleDialogHarness from './AccessibleDialogHarness.svelte';

describe('accessible dialog behavior', () => {
  it('moves focus into the dialog, traps it, and restores the trigger', async () => {
    const view = render(AccessibleDialogHarness);
    const trigger = view.getByRole('button', { name: 'Open dialog' });
    trigger.focus();
    await fireEvent.click(trigger);

    const dialog = view.getByRole('dialog', { name: 'Test dialog' });
    const close = view.getByRole('button', { name: 'Close dialog' });
    const last = view.getByRole('button', { name: 'Last action' });
    await waitFor(() => expect(document.activeElement).toBe(close));
    expect(view.container.querySelector('main')?.inert).toBe(true);

    last.focus();
    await fireEvent.keyDown(dialog, { key: 'Tab' });
    expect(document.activeElement).toBe(close);

    await fireEvent.keyDown(dialog, { key: 'Escape' });
    expect(view.queryByRole('dialog')).toBeNull();
    expect(document.activeElement).toBe(trigger);
    expect(view.container.querySelector('main')?.inert).toBe(false);
  });
});
