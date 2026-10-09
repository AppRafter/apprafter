// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What a locked app shows (spec §4.3, brief §3): why it is locked, whose account unlocks it, and
// how. Where the OS prompts (AuthInfo.passwordField false), one Unlock button: Rust asks the OS
// for the owner. Where it cannot — Linux without a polkit agent or policy — the app's own
// password field instead, checked by Rust (`unlock_with_password`); the plain button would only
// ask for a prompt that cannot show. Whether the field shows is app_info's to say, read again on
// every lock and after every refusal (state/platform.ts).
//
// The password lives in the field's state while it is typed, goes out with the request, and the
// field is emptied when the answer comes, whatever it is: it is never cached, mutated through
// the query client or logged. A wrong password shows what the OS said (PAM's messages) or a
// plain line under the field, which is marked until the owner types again; too many failures
// hold the field for the back-off, saying why.
import { type FormEvent, type ReactNode, useEffect, useRef, useState } from 'react';
import { Button } from '../components/Button';
import { ArrowRightIcon, SpinnerGapIcon } from '../components/icons';
import { Logo } from '../components/Logo';
import { PasswordField } from '../components/PasswordField';
import { Tag } from '../components/Tag';
import { uiErrorOf } from '../ipc/api';
import type { LockState } from '../ipc/generated/LockState';
import type { SecretBackend } from '../ipc/generated/SecretBackend';
import { authPrompt, authRefusal, BACKOFF_LINE, BACKOFF_MS, useBackoff } from '../state/auth';
import { useLockActions } from '../state/lock';
import { osName, usePlatform } from '../state/platform';

function reasonLine({ reason, autoLockMinutes: minutes }: LockState): string {
  switch (reason) {
    case 'startup':
      return minutes === null
        ? 'Locked when the app started.'
        : `Locked when the app started. Auto-lock after ${minutes} min idle.`;
    case 'idle':
      return minutes === null
        ? 'Locked after a period of inactivity.'
        : `Locked after ${minutes} minutes of inactivity.`;
    case 'os_session':
      return 'Locked when the computer locked or slept.';
    case 'manual':
    case null:
      return 'Locked.';
  }
}

const FOOTERS: Record<SecretBackend, string> = {
  file: 'Clusters keep running. Credentials are stored in files on this computer.',
  keyring: 'Clusters keep running. Credentials stay in the system keychain.',
};

/** "alex.morgan" → "AM": the first letters of the account's first two words. */
function initials(account: string): string {
  const words = account.split(/[^\p{L}\p{N}]+/u).filter((word) => word !== '');
  return (
    words
      .slice(0, 2)
      .map((word) => word[0]?.toUpperCase() ?? '')
      .join('') || '?'
  );
}

export interface LockScreenProps {
  state: LockState;
  /** How long too many failed passwords hold the field; tests pass their own. */
  backoffMs?: number;
}

export function LockScreen({ state, backoffMs = BACKOFF_MS }: LockScreenProps) {
  const info = usePlatform();
  const { unlock, unlockWithPassword } = useLockActions();
  const [waiting, setWaiting] = useState(false);
  const [refusal, setRefusal] = useState<readonly string[]>([]);
  const [backoff, startBackoff] = useBackoff(backoffMs);
  const method = info.auth.method;

  const onUnlock = async () => {
    setWaiting(true);
    setRefusal([]);
    try {
      await unlock();
    } catch (error) {
      setRefusal(authRefusal(uiErrorOf(error), false).lines);
      setWaiting(false);
    }
  };

  // Resolves once the answer is in (never rejects): the field then empties itself.
  const onPassword = async (password: string) => {
    setWaiting(true);
    setRefusal([]);
    try {
      await unlockWithPassword(password);
    } catch (error) {
      const refused = authRefusal(uiErrorOf(error), true);
      setRefusal(refused.lines);
      if (refused.backoff) startBackoff();
      setWaiting(false);
    }
  };

  const said = (refusal.length > 0 || backoff) && (
    <div className="lock-refusal" role="alert">
      {refusal.map((line, index) => (
        // A fixed list per answer, replaced whole: the index is a stable key.
        // biome-ignore lint/suspicious/noArrayIndexKey: see above
        <p key={index}>{line}</p>
      ))}
      {backoff && <p>{BACKOFF_LINE}</p>}
    </div>
  );

  return (
    <main className="lock-screen">
      <div className="lock-column">
        <Logo size={56} />
        <h1 className="lock-title">AppRafter is locked</h1>
        <p className="lock-reason">{reasonLine(state)}</p>
        <div className="lock-account">
          <span className="lock-avatar" aria-hidden="true">
            {initials(info.account)}
          </span>
          <span className="lock-account-text">
            <span className="lock-account-name">{info.account}</span>
            <span className="lock-account-meta">{`${osName(info.os)} account · ${info.host}`}</span>
          </span>
        </div>
        {info.auth.passwordField ? (
          <PasswordForm
            account={info.account}
            checking={waiting}
            backoff={backoff}
            refused={refusal.length > 0}
            onType={() => setRefusal((lines) => (lines.length === 0 ? lines : []))}
            onSubmit={onPassword}
          >
            {said}
          </PasswordForm>
        ) : (
          <>
            <Button variant="primary" size={36} full disabled={waiting} onClick={onUnlock}>
              {waiting && method !== null ? `Waiting for ${authPrompt(method)}…` : 'Unlock'}
            </Button>
            {said}
          </>
        )}
        {info.testBuild && <Tag tone="warn">TEST BUILD</Tag>}
      </div>
      <footer className="lock-footer">{FOOTERS[info.secretBackend]}</footer>
    </main>
  );
}

interface PasswordFormProps {
  account: string;
  /** A check is running: the field cannot change and nothing is sent again. */
  checking: boolean;
  /** Too many failed attempts: the field waits. */
  backoff: boolean;
  /** The last attempt was refused: the field is marked (aria-invalid) until the owner types. */
  refused: boolean;
  onType: () => void;
  onSubmit: (password: string) => Promise<void>;
  /** What the refusal says, under the field. */
  children: ReactNode;
}

/**
 * The field, its arrow and what a refusal says under them (the design's lock screen). Its own
 * component, so the password state goes with it whenever the field does.
 */
function PasswordForm({
  account,
  checking,
  backoff,
  refused,
  onType,
  onSubmit,
  children,
}: PasswordFormProps) {
  const input = useRef<HTMLInputElement>(null);
  const [password, setPassword] = useState('');
  const blocked = checking || backoff;

  // In reach at once, and again once the back-off ends (a disabled field loses the focus).
  useEffect(() => {
    if (!backoff) input.current?.focus();
  }, [backoff]);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    // Rust counts an empty password as a wrong one: never send it.
    if (password === '' || blocked) return;
    void onSubmit(password).finally(() => setPassword(''));
  };

  return (
    <div className="lock-password">
      <form className="lock-password-row" onSubmit={submit}>
        <PasswordField
          ref={input}
          label="System password"
          placeholder={`Password for ${account}`}
          value={password}
          onChange={(value) => {
            setPassword(value);
            onType();
          }}
          mono={false}
          background="surface"
          readOnly={checking}
          disabled={backoff}
          aria-invalid={refused || undefined}
        />
        <Button
          type="submit"
          variant="primary"
          size={36}
          aria-label="Unlock"
          title="Unlock"
          aria-busy={checking || undefined}
          disabled={password === '' || blocked}
        >
          {checking ? (
            <SpinnerGapIcon className="spin" aria-hidden="true" />
          ) : (
            <ArrowRightIcon aria-hidden="true" />
          )}
        </Button>
      </form>
      {children}
    </div>
  );
}
