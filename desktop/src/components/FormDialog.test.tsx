// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, mock, test } from 'bun:test';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { IpcError } from '../ipc/api';
import { FormDialog, type FormSpec, type FormValues } from './FormDialog';

interface Deferred {
  promise: Promise<void>;
  resolve: () => void;
  reject: (reason: unknown) => void;
}

function deferred(): Deferred {
  let resolve!: () => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<void>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function open(spec: Partial<FormSpec> & Pick<FormSpec, 'fields'>, onClose = mock()) {
  const onSubmit = mock<(values: FormValues) => Promise<void> | void>();
  const full: FormSpec = { title: 'Backup schedule', onSubmit, ...spec };
  render(<FormDialog {...full} onClose={onClose} />);
  return { user: userEvent.setup(), onSubmit: full.onSubmit as typeof onSubmit, onClose };
}

const submit = () => screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement;

describe('FormDialog', () => {
  test('starts each kind from its default, and submits what it shows', async () => {
    const { user, onSubmit } = open({
      fields: [
        { key: 'name', label: 'Name', def: 'nightly' },
        { key: 'note', label: 'Note' },
        {
          key: 'every',
          label: 'Every',
          kind: 'seg',
          options: [['1h', 'Hourly'], 'daily'],
          def: 'daily',
        },
        { key: 'keep', label: 'Keep', kind: 'chips', options: ['db', 'disk'] },
        { key: 'on', label: 'Enabled', kind: 'toggle', toggleLabel: 'Run on schedule' },
      ],
    });
    expect((screen.getByLabelText('Name') as HTMLInputElement).value).toBe('nightly');
    expect(screen.getByRole('radio', { name: 'daily' }).getAttribute('aria-checked')).toBe('true');
    await user.click(screen.getByRole('button', { name: 'disk' }));
    await user.click(submit());
    expect(onSubmit).toHaveBeenCalledWith({
      name: 'nightly',
      note: '',
      every: 'daily',
      keep: ['disk'],
      on: false,
    });
  });

  test("a required field blocks submit while it is '' — and a required toggle while off", async () => {
    const { user } = open({
      fields: [
        { key: 'name', label: 'Name' },
        { key: 'saved', label: 'Passphrase', kind: 'toggle', toggleLabel: "I've saved it" },
      ],
      required: ['name', 'saved'],
    });
    expect(submit().disabled).toBe(true);
    await user.type(screen.getByLabelText('Name'), 'nightly');
    expect(submit().disabled).toBe(true);
    await user.click(screen.getByRole('switch', { name: "I've saved it" }));
    expect(submit().disabled).toBe(false);
  });

  test('a hidden field is not required, and its value is left out of the submission', async () => {
    const { user, onSubmit } = open({
      fields: [
        { key: 'target', label: 'Target', kind: 'seg', options: ['local', 's3'], def: 'local' },
        { key: 'bucket', label: 'Bucket', def: 'old', when: (v) => v.target === 's3' },
        { key: 'endpoint', label: 'Endpoint', when: (v) => v.target === 's3' },
      ],
      required: ['endpoint'],
    });
    expect(screen.queryByLabelText('Bucket')).toBeNull();
    expect(submit().disabled).toBe(false);
    await user.click(submit());
    expect(onSubmit).toHaveBeenCalledWith({ target: 'local' });

    await user.click(screen.getByRole('radio', { name: 's3' }));
    expect(screen.getByLabelText('Bucket')).toBeDefined();
    expect(submit().disabled).toBe(true);
  });

  test('a password field reveals its own value only', async () => {
    const { user } = open({
      fields: [
        { key: 'token', label: 'Token', type: 'password' },
        { key: 'key', label: 'Key', type: 'password' },
      ],
    });
    await user.click(screen.getByRole('button', { name: 'Show Token' }));
    expect((screen.getByLabelText('Token') as HTMLInputElement).type).toBe('text');
    expect((screen.getByLabelText('Key') as HTMLInputElement).type).toBe('password');
  });

  test('stays open while onSubmit runs, then closes', async () => {
    const running = deferred();
    const onClose = mock();
    const user = userEvent.setup();
    render(
      <FormDialog
        title="Rename"
        fields={[{ key: 'name', label: 'Name', def: 'x' }]}
        onSubmit={() => running.promise}
        onClose={onClose}
      />,
    );
    await user.click(submit());
    expect(screen.getByRole('dialog')).toBeDefined();
    expect(submit().disabled).toBe(true);
    expect((screen.getByRole('button', { name: 'Cancel' }) as HTMLButtonElement).disabled).toBe(
      true,
    );
    await user.keyboard('{Escape}');
    expect(onClose).not.toHaveBeenCalled();
    running.resolve();
    await new Promise((r) => setTimeout(r, 0));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  test('a rejection is shown inline and the dialog stays, ready to try again', async () => {
    const onClose = mock();
    const user = userEvent.setup();
    const error = {
      code: 'apprafter::backup::job_active',
      message: 'A backup is running.',
      help: null,
      causes: [],
      fields: {},
    };
    render(
      <FormDialog
        title="Backup"
        fields={[{ key: 'name', label: 'Name', def: 'x' }]}
        onSubmit={() => Promise.reject(new IpcError('op_execute', error))}
        onClose={onClose}
      />,
    );
    await user.click(submit());
    expect(screen.getByRole('alert').textContent).toContain('A backup is running.');
    expect(onClose).not.toHaveBeenCalled();
    expect(submit().disabled).toBe(false);
  });

  test('a backdrop click never dismisses it (typed secrets would go)', async () => {
    const { user, onClose } = open({
      fields: [{ key: 'token', label: 'Token', type: 'password' }],
    });
    await user.click(document.querySelector('.modal-layer') as HTMLElement);
    expect(onClose).not.toHaveBeenCalled();
    await user.keyboard('{Escape}');
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});
