// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What a locked app shows (spec §4.3, brief §3): why it is locked, whose account unlocks it, and
// one Unlock button — Rust asks the OS for the owner. The webview's own password field belongs
// to the PAM path and needs `unlock_with_password` (D.2d); that command does not exist yet, so
// no field is shown rather than one that cannot unlock anything.
import { useState } from 'react';
import { Button } from '../components/Button';
import { Logo } from '../components/Logo';
import { Tag } from '../components/Tag';
import { uiErrorOf } from '../ipc/api';
import type { LockState } from '../ipc/generated/LockState';
import type { SecretBackend } from '../ipc/generated/SecretBackend';
import { authPrompt } from '../state/auth';
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

export function LockScreen({ state }: { state: LockState }) {
  const info = usePlatform();
  const { unlock } = useLockActions();
  const [waiting, setWaiting] = useState(false);
  const [refusal, setRefusal] = useState<string | null>(null);
  const method = info.auth.method;

  const onUnlock = async () => {
    setWaiting(true);
    setRefusal(null);
    try {
      await unlock();
    } catch (error) {
      setRefusal(uiErrorOf(error).message);
      setWaiting(false);
    }
  };

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
        <Button variant="primary" size={36} full disabled={waiting} onClick={onUnlock}>
          {waiting && method !== null ? `Waiting for ${authPrompt(method)}…` : 'Unlock'}
        </Button>
        {refusal !== null && (
          <p className="lock-refusal" role="alert">
            {refusal}
          </p>
        )}
        {info.testBuild && <Tag tone="warn">TEST BUILD</Tag>}
      </div>
      <footer className="lock-footer">{FOOTERS[info.secretBackend]}</footer>
    </main>
  );
}
