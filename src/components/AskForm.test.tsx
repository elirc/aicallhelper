import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { AskForm } from './AskForm';

/**
 * Counts how often the PracticeLibrary chunk is LOADED (the factory runs once,
 * on the first import). The chunk is deliberately NOT imported at the top of
 * this file: the first describe proves a hover fetches it before any click,
 * which is only provable while nothing else has loaded it yet. Every later
 * `findBy*` that waits for the library gets a long timeout instead — a cold
 * jsdom can take more than the default second to compile the chunk.
 */
const loads = vi.hoisted(() => ({ count: 0 }));
vi.mock('./PracticeLibrary', async (importOriginal) => {
  loads.count += 1;
  return importOriginal<typeof import('./PracticeLibrary')>();
});

const CHUNK_TIMEOUT = { timeout: 10_000 };

describe('chunk preloading (FE-3)', () => {
  it('fetches the PracticeLibrary chunk on hover, before any click opens it', async () => {
    const user = userEvent.setup();
    render(<AskForm disabled={false} onAsk={vi.fn(async () => true)} />);
    expect(loads.count).toBe(0);
    await user.hover(screen.getByRole('button', { name: /Interview prep/ }));
    await waitFor(() => expect(loads.count).toBe(1), CHUNK_TIMEOUT);
    // Warmed, not opened.
    expect(screen.getByRole('button', { name: /Interview prep/ })).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByRole('button', { name: /Tell me about yourself/ })).not.toBeInTheDocument();
  });
});

describe('interview preparation', () => {
  it('stages a question for editing without sending it, then submits the edited draft', async () => {
    const user = userEvent.setup();
    const onAsk = vi.fn(async () => true);
    render(<AskForm disabled={false} onAsk={onAsk} />);
    expect(screen.queryByRole('region', { name: 'Interview preparation' })).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /Interview prep/ }));
    await user.click(await screen.findByRole('button', { name: /Tell me about yourself/ }, CHUNK_TIMEOUT));
    const input = screen.getByLabelText('Type a question');
    expect(input).toHaveValue('Tell me about yourself.');
    expect(input).toHaveFocus();
    expect(onAsk).not.toHaveBeenCalled();
    await user.type(input, ' Focus on my recent work.');
    await user.click(screen.getByRole('button', { name: 'Ask' }));
    expect(onAsk).toHaveBeenCalledWith('Tell me about yourself. Focus on my recent work.');
    expect(input).toHaveValue('');
  });

  it('filters questions by category and search, and can reset an empty result', async () => {
    const user = userEvent.setup();
    render(<AskForm disabled={false} onAsk={vi.fn(async () => true)} />);
    await user.click(screen.getByRole('button', { name: /Interview prep/ }));
    await user.selectOptions(await screen.findByLabelText('Question category', {}, CHUNK_TIMEOUT), 'Technical');
    expect(screen.getByText('3 of 12 questions')).toBeInTheDocument();
    await user.type(screen.getByLabelText('Search practice questions'), 'slow');
    expect(screen.getByText('1 of 12 questions')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /investigate a slow application/ })).toBeInTheDocument();
    await user.type(screen.getByLabelText('Search practice questions'), 'xyz');
    expect(screen.getByText('0 of 12 questions')).toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: 'Reset filters' }));
    expect(screen.getByText('12 of 12 questions')).toBeInTheDocument();
    expect(screen.getByLabelText('Search practice questions')).toHaveValue('');
    expect(screen.getByLabelText('Question category')).toHaveValue('All');
  });

  it('keeps guides available during recording and disables question selection', async () => {
    const user = userEvent.setup();
    const onAsk = vi.fn(async () => true);
    const { rerender } = render(<AskForm disabled={false} onAsk={onAsk} />);
    await user.click(screen.getByRole('button', { name: /Interview prep/ }));
    const question = await screen.findByRole('button', { name: /Tell me about yourself/ }, CHUNK_TIMEOUT);
    rerender(<AskForm disabled onAsk={onAsk} />);
    expect(question).toBeDisabled();
    await user.click(question);
    expect(onAsk).not.toHaveBeenCalled();
    await user.click(screen.getByText('Before the call'));
    expect(screen.getByText(/Capture uses system audio/)).toBeVisible();
    expect(screen.getByLabelText('Type a question')).toHaveValue('');
  });
});

describe('non-interview profiles', () => {
  // The library is interview-only content; a sales/support profile hides the
  // toggle and must also close a library the user left open, or it lingers
  // under a toggle that no longer exists.
  it('hides the Interview-prep toggle and closes an open library when showPrep turns off', async () => {
    const user = userEvent.setup();
    const onAsk = vi.fn(async () => true);
    const { rerender } = render(<AskForm disabled={false} onAsk={onAsk} showPrep />);
    await user.click(screen.getByRole('button', { name: /Interview prep/ }));
    await screen.findByRole('button', { name: /Tell me about yourself/ }, CHUNK_TIMEOUT);
    rerender(<AskForm disabled={false} onAsk={onAsk} showPrep={false} />);
    expect(screen.queryByRole('button', { name: /Interview prep/ })).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Tell me about yourself/ })).not.toBeInTheDocument();
    // Back to an interview profile: the toggle returns closed, not sprung open.
    rerender(<AskForm disabled={false} onAsk={onAsk} showPrep />);
    expect(screen.getByRole('button', { name: /Interview prep/ })).toHaveAttribute('aria-expanded', 'false');
    expect(screen.queryByRole('button', { name: /Tell me about yourself/ })).not.toBeInTheDocument();
    // The typed draft survives the whole trip.
    expect(screen.getByLabelText('Type a question')).toBeEnabled();
  });
});

describe('pending typed questions', () => {
  it('sends once and preserves a rejected draft for retry', async () => {
    let settle!: (accepted: boolean) => void;
    const onAsk = vi.fn(() => new Promise<boolean>((resolve) => { settle = resolve; }));
    render(<AskForm disabled={false} onAsk={onAsk} />);
    const input = screen.getByLabelText('Type a question');
    fireEvent.change(input, { target: { value: 'My draft question' } });
    const form = input.closest('form')!;
    act(() => { fireEvent.submit(form); fireEvent.submit(form); });
    expect(onAsk).toHaveBeenCalledTimes(1);
    expect(input).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Sending…' })).toBeDisabled();
    await act(async () => settle(false));
    await waitFor(() => expect(input).toBeEnabled());
    expect(input).toHaveValue('My draft question');
    expect(screen.getByRole('button', { name: 'Ask' })).toBeEnabled();
  });
});
