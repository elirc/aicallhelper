import { describe, expect, it } from 'vitest';
import { render, screen } from '@testing-library/react';
import type { ErrorCode } from '../types';
import { ErrorBox } from './ErrorBox';

const RENDERED_CODES: ErrorCode[] = [
  'no_stt_key',
  'no_llm_key',
  'stt_connect',
  'stt_error',
  'stt_timeout',
  'no_speech',
  'llm_auth',
  'llm_http',
  'llm_rate_limit',
  'llm_first_token_timeout',
  'llm_timeout',
  'internal',
];

describe('ErrorBox', () => {
  it.each(RENDERED_CODES)('renders %s as an alert carrying the message', (code) => {
    render(<ErrorBox error={{ code, message: `detail for ${code}` }} />);
    const alert = screen.getByRole('alert');
    expect(alert).toHaveTextContent(`detail for ${code}`);
    // Every code has a human title in front of the raw message.
    expect(alert.textContent?.length ?? 0).toBeGreaterThan(`detail for ${code}`.length);
  });

  it('renders nothing for aborted — the user did that on purpose', () => {
    const { container } = render(<ErrorBox error={{ code: 'aborted', message: 'cancelled' }} />);
    expect(container).toBeEmptyDOMElement();
  });

  it('renders nothing when there is no error', () => {
    const { container } = render(<ErrorBox error={null} />);
    expect(container).toBeEmptyDOMElement();
  });
});
