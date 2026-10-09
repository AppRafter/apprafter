// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Card, PageHeader, PageGrid, PlanChanges: the Target screen's frame, and the changes a plan lists.
import { describe, expect, test } from 'bun:test';
import { render, screen, within } from '@testing-library/react';
import type { PlannedChange } from '../ipc/generated/PlannedChange';
import { Card } from './Card';
import { PageGrid } from './PageGrid';
import { PageHeader } from './PageHeader';
import { kindLabel, PlanChanges } from './PlanChanges';

describe('Card', () => {
  test('a titled region; danger marks it for the stylesheet', () => {
    render(
      <Card title="Danger zone" tone="danger">
        <p>body</p>
      </Card>,
    );
    const region = screen.getByRole('region', { name: 'Danger zone' });
    expect(region.getAttribute('data-tone')).toBe('danger');
    expect(within(region).getByRole('heading', { level: 2, name: 'Danger zone' })).toBeDefined();
    expect(within(region).getByText('body')).toBeDefined();
  });

  test('a note sits beside the title; without a tone the card is plain', () => {
    render(
      <Card title="Target" note="read 5 s ago">
        <p>body</p>
      </Card>,
    );
    const region = screen.getByRole('region', { name: 'Target' });
    expect(region.getAttribute('data-tone')).toBe('default');
    expect(within(region).getByText('read 5 s ago').className).toBe('card-note');
  });
});

describe('PageHeader', () => {
  test('title, sub, and the actions slot', () => {
    render(
      <PageHeader
        title="Target"
        sub="How this computer reaches prod"
        actions={<button type="button">Run doctor</button>}
      />,
    );
    expect(screen.getByRole('heading', { level: 1, name: 'Target' })).toBeDefined();
    expect(screen.getByText('How this computer reaches prod')).toBeDefined();
    expect(
      screen.getByRole('button', { name: 'Run doctor' }).closest('.page-actions'),
    ).not.toBeNull();
  });

  test('no actions, no slot', () => {
    const { container } = render(<PageHeader title="Target" />);
    expect(container.querySelector('.page-actions')).toBeNull();
    expect(container.querySelector('.page-header')?.hasAttribute('data-actions')).toBe(false);
  });
});

describe('PageGrid', () => {
  test('names its layout for the stylesheet', () => {
    const { container } = render(
      <PageGrid layout="main-side">
        <div />
        <div />
      </PageGrid>,
    );
    expect(container.querySelector('.page-grid')?.getAttribute('data-layout')).toBe('main-side');
  });
});

describe('PlanChanges', () => {
  const changes: PlannedChange[] = [
    { kind: 'Target', object: 'prod', action: 'rename', detail: 'prod → prod-eu' },
    {
      kind: 'LocalState',
      object: 'prod',
      action: 'delete',
      detail: 'records server prod-1 (id 4711); the server keeps running at the provider',
    },
    { kind: 'CliDefault', object: 'prod', action: 'clear_default', detail: null },
  ];

  test('one row per change: what happens, to what, and the detail', () => {
    render(<PlanChanges changes={changes} />);
    expect(screen.getAllByRole('listitem').map((li) => li.textContent)).toEqual([
      'RenameTarget prodprod → prod-eu',
      'DeleteLocal state prodrecords server prod-1 (id 4711); the server keeps running at the provider',
      'Clear defaultCLI default prod',
    ]);
  });

  test('a delete reads as an error, a rename as a warning', () => {
    render(<PlanChanges changes={changes} />);
    const tones = screen
      .getAllByRole('listitem')
      .map((li) => li.querySelector('.tag')?.getAttribute('data-tone'));
    expect(tones).toEqual(['warn', 'err', 'warn']);
  });

  test('nothing to change says so', () => {
    render(<PlanChanges changes={[]} />);
    expect(screen.getByText('Nothing changes.')).toBeDefined();
    expect(screen.queryByRole('list')).toBeNull();
  });

  test('a kind the core adds later shows as it is', () => {
    expect(kindLabel('Credentials')).toBe('Credentials');
    expect(kindLabel('LocalState')).toBe('Local state');
    expect(kindLabel('Firewall')).toBe('Firewall');
    expect(kindLabel('toString')).toBe('toString');
  });
});
