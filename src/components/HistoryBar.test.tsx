import { describe, expect, it, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import type { ComponentProps } from 'react';
import { HistoryBar } from './HistoryBar';

type BarProps = ComponentProps<typeof HistoryBar>;

function renderBar(overrides: Partial<BarProps> = {}) {
  const props: BarProps = {
    historyLength: 2,
    viewIndex: 0,
    idle: true,
    onPrev: vi.fn(),
    onNext: vi.fn(),
    onClear: vi.fn(),
    ...overrides,
  };
  return { ...render(<HistoryBar {...props} />), props };
}

describe('HistoryBar', () => {
  it('hides below two entries', () => {
    renderBar({ historyLength: 1 });
    expect(screen.queryByRole('navigation')).not.toBeInTheDocument();
  });

  it('shows the n/m position label', () => {
    renderBar({ viewIndex: 1 });
    expect(screen.getByText('2/2')).toBeInTheDocument();
  });

  it('disables the arrows at the ends and fires them in between', async () => {
    const user = userEvent.setup();
    const { props } = renderBar({ historyLength: 3, viewIndex: 1 });
    await user.click(screen.getByRole('button', { name: 'Previous answer' }));
    await user.click(screen.getByRole('button', { name: 'Next answer' }));
    expect(props.onPrev).toHaveBeenCalledTimes(1);
    expect(props.onNext).toHaveBeenCalledTimes(1);
  });

  it('disables prev at the oldest and next at the newest', () => {
    renderBar({ viewIndex: 0 });
    expect(screen.getByRole('button', { name: 'Previous answer' })).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Next answer' })).toBeEnabled();
  });

  it('permits Clear only while idle', () => {
    renderBar({ idle: false });
    expect(screen.getByRole('button', { name: 'Clear' })).toBeDisabled();
  });

  it('fires onClear', async () => {
    const user = userEvent.setup();
    const { props } = renderBar();
    await user.click(screen.getByRole('button', { name: 'Clear' }));
    expect(props.onClear).toHaveBeenCalledTimes(1);
  });
});
