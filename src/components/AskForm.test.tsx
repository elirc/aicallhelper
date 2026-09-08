// Compile optional views during test setup, before timed UI interactions.
import './PracticeLibrary';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';
import { AskForm } from './AskForm';

describe('interview preparation', () => {
  it('stages a question for editing without sending it, then submits the edited draft', async () => {
    const user = userEvent.setup();
    const onAsk = vi.fn(async () => true);
    render(<AskForm disabled={false} onAsk={onAsk} />);
    expect(screen.queryByRole('region', { name: 'Interview preparation' })).not.toBeInTheDocument();
    await user.click(screen.getByRole('button', { name: /Interview prep/ }));
    await user.click(await screen.findByRole('button', { name: /Tell me about yourself/ }));
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
    await user.selectOptions(await screen.findByLabelText('Question category'), 'Technical');
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
    const question = await screen.findByRole('button', { name: /Tell me about yourself/ });
    rerender(<AskForm disabled onAsk={onAsk} />);
    expect(question).toBeDisabled();
    await user.click(question);
    expect(onAsk).not.toHaveBeenCalled();
    await user.click(screen.getByText('Before the call'));
    expect(screen.getByText(/Capture uses system audio/)).toBeVisible();
    expect(screen.getByLabelText('Type a question')).toHaveValue('');
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
