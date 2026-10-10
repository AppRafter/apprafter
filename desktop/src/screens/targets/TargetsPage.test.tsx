// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, mock, test } from 'bun:test';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { MOCK_TARGETS } from '../../ipc/mock/fixtures';
import { TargetsPage } from './TargetsPage';
import { TargetsSource } from './targets';

describe('TargetsPage', () => {
  test('says where targets live, and that nothing can list them yet', () => {
    render(<TargetsPage onOpen={() => {}} />);
    expect(screen.getByRole('heading', { name: 'Open a cluster' })).toBeDefined();
    expect(
      screen.getByText('Targets live in your local target store. Each opens in its own tab.'),
    ).toBeDefined();
    expect(
      screen.getByRole('heading', {
        name: 'No data source yet — target commands arrive in D.3',
      }),
    ).toBeDefined();
  });

  test('with a source, one card per target; a click opens it', async () => {
    const onOpen = mock();
    render(
      <TargetsSource value={MOCK_TARGETS}>
        <TargetsPage onOpen={onOpen} />
      </TargetsSource>,
    );
    const card = screen.getByRole('button', { name: /prod-eu/ });
    expect(card.textContent).toContain('hetzner-cloud');
    expect(card.textContent).toContain('nbg1');
    expect(card.textContent).toContain('Team (T2)');
    expect(card.textContent).toContain('CLI default');
    expect(screen.getByRole('button', { name: /lab/ }).textContent).toContain('Solo (T1)');
    expect(screen.getByRole('button', { name: /lab/ }).textContent).not.toContain('CLI default');
    await userEvent.setup().click(card);
    expect(onOpen).toHaveBeenCalledWith('prod-eu');
  });

  test('adding a target is shown, disabled, until D.3', () => {
    render(<TargetsPage onOpen={() => {}} />);
    const add = screen.getByRole('button', { name: /Add target/ }) as HTMLButtonElement;
    expect(add.disabled).toBe(true);
    expect(add.textContent).toContain('Arrives in D.3');
  });
});
