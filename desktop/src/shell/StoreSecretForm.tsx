// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// The lock screen of a protected store (D.4a T75, D17): the store's lock password or its
// recovery key, in the app's own field on every OS — no system prompt yields a key in D.4a, so
// neither Windows Hello, Touch ID nor polkit opens the vault, and PAM is not asked. Rust tries a
// recovery key's shape against the recovery slot and anything else against the password slot
// (T62). The field is emptied after every answer, whatever it is, and never cached. Below it,
// `gesture`: D.4b's slot that yields a key (Windows Hello first), under an "or", as the design's
// lock screen draws it; nothing in D.4a.
import { type FormEvent, type ReactNode, useEffect, useRef, useState } from 'react';
import { Button } from '../components/Button';
import { ArrowRightIcon, SpinnerGapIcon } from '../components/icons';
import { PasswordField } from '../components/PasswordField';
import { STORE_FIELD_LABEL, STORE_FIELD_PLACEHOLDER, storeLine } from '../state/lockText';

export interface StoreSecretFormProps {
  /** The store's root as the app shows it. */
  readonly store: string;
  /** A check runs: the field cannot change and nothing is sent again. */
  readonly checking: boolean;
  /** Too many failed attempts: the field waits out Rust's back-off. */
  readonly backoff: boolean;
  /** The last attempt was refused: the field is marked until the owner types again. */
  readonly refused: boolean;
  readonly onType: () => void;
  readonly onSubmit: (secret: string) => Promise<void>;
  /** D.4b: a gesture slot's button, under an "or". */
  readonly gesture?: ReactNode;
  /** What the refusal says, under the field. */
  readonly children: ReactNode;
}

export function StoreSecretForm(p: StoreSecretFormProps) {
  const input = useRef<HTMLInputElement>(null);
  const [secret, setSecret] = useState('');
  const blocked = p.checking || p.backoff;

  useEffect(() => {
    if (!p.backoff) input.current?.focus();
  }, [p.backoff]);

  const submit = (event: FormEvent) => {
    event.preventDefault();
    // An empty answer opens nothing and would count as a wrong one: never sent.
    if (secret === '' || blocked) return;
    void p.onSubmit(secret).finally(() => setSecret(''));
  };

  return (
    <div className="lock-password">
      <p className="lock-store">{storeLine(p.store)}</p>
      <form className="lock-password-row" onSubmit={submit}>
        <PasswordField
          ref={input}
          label={STORE_FIELD_LABEL}
          placeholder={STORE_FIELD_PLACEHOLDER}
          value={secret}
          onChange={(value) => {
            setSecret(value);
            p.onType();
          }}
          mono={false}
          background="surface"
          readOnly={p.checking}
          disabled={p.backoff}
          aria-invalid={p.refused || undefined}
        />
        <Button
          type="submit"
          variant="primary"
          size={36}
          aria-label="Unlock"
          title="Unlock"
          aria-busy={p.checking || undefined}
          disabled={secret === '' || blocked}
        >
          {p.checking ? (
            <SpinnerGapIcon className="spin" aria-hidden="true" />
          ) : (
            <ArrowRightIcon aria-hidden="true" />
          )}
        </Button>
      </form>
      {p.children}
      {p.gesture !== undefined && (
        <>
          <div className="lock-or">or</div>
          {p.gesture}
        </>
      )}
    </div>
  );
}
