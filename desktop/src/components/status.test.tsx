// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Badge, Dot, StatusPill, Tag, Eyebrow, Kbd.
import { describe, expect, test } from 'bun:test';
import { render, screen } from '@testing-library/react';
import { Badge } from './Badge';
import { Dot } from './Dot';
import { Eyebrow } from './Eyebrow';
import { Kbd } from './Kbd';
import { StatusPill } from './StatusPill';
import { Tag } from './Tag';

describe('Badge', () => {
  test('renders nothing at zero', () => {
    const { container } = render(<Badge count={0} label="approvals waiting" />);
    expect(container.innerHTML).toBe('');
  });

  test('shows the count, and a screen reader hears it as a sentence', () => {
    const { container } = render(<Badge count={3} label="approvals waiting" />);
    const badge = container.firstElementChild as HTMLElement;
    expect(badge.dataset.tone).toBe('warn');
    expect(screen.getByText('3').getAttribute('aria-hidden')).toBe('true');
    expect(screen.getByText('3 approvals waiting').className).toBe('sr-only');
  });

  test('caps a large count at 99+', () => {
    render(<Badge count={250} tone="accent" label="unread" />);
    expect(screen.getByText('99+').getAttribute('aria-hidden')).toBe('true');
    expect(screen.getByText('250 unread')).toBeDefined();
  });
});

describe('Dot', () => {
  test('is unknown until told otherwise, and decoration without a label', () => {
    const { container } = render(<Dot />);
    const dot = container.firstElementChild as HTMLElement;
    expect(dot.dataset.tone).toBe('unknown');
    expect(dot.getAttribute('aria-hidden')).toBe('true');
  });

  test('with a label it is an image a screen reader names', () => {
    render(<Dot tone="ok" label="Healthy" />);
    expect(screen.getByRole('img', { name: 'Healthy' }).dataset.tone).toBe('ok');
  });
});

test('StatusPill, Tag, Eyebrow and Kbd carry their tone, variant and text', () => {
  render(
    <>
      <StatusPill tone="err" dot>
        Degraded
      </StatusPill>
      <Tag tone="warn">Bounded</Tag>
      <Tag variant="chip" mono>
        prod
      </Tag>
      <Eyebrow as="label" htmlFor="x">
        Token
      </Eyebrow>
      <input id="x" />
      <Kbd>Ctrl+L</Kbd>
    </>,
  );
  const pill = screen.getByText('Degraded');
  expect(pill.dataset.tone).toBe('err');
  expect(pill.querySelector('.pill-dot')).not.toBeNull();
  expect(screen.getByText('Bounded').dataset.variant).toBe('tinted');
  expect(screen.getByText('prod').dataset.variant).toBe('chip');
  expect(screen.getByLabelText('Token').tagName).toBe('INPUT');
  expect(screen.getByText('Ctrl+L').tagName).toBe('KBD');
});
