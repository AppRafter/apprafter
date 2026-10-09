// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Add target (spec §7, target add): Provider → Machine → Details, then "Save target", which is
// the Bounded plan's plain confirm (spec §4.4). The token is sent once, to verify it; Rust keeps it
// as a draft and the field is emptied (decision 6). The catalogue and the plan name the draft.
// No Paste (no clipboard read, R11), no --force, no --no-ping (decision 8), no provisioning (D.12).
import { useEffect, useReducer, useRef, useState } from 'react';
import { Button } from '../../components/Button';
import { ChoiceCardGroup } from '../../components/ChoiceCardGroup';
import { ErrorPanel } from '../../components/ErrorPanel';
import { CheckCircleIcon } from '../../components/icons';
import { PasswordField } from '../../components/PasswordField';
import { Wizard } from '../../components/Wizard';
import * as api from '../../ipc/api';
import type { DraftId } from '../../ipc/generated/DraftId';
import { DESKTOP_ERROR_CODES } from '../../ipc/generated/errors';
import type { TokenVerified } from '../../ipc/generated/TokenVerified';
import { HETZNER_TOKEN_LEN, SUPPORTED_PROVIDERS } from '../../ipc/generated/target';
import type { UiError } from '../../ipc/generated/UiError';
import { reportUnlessLocked } from '../../ipc/plans';
import { usePlatform } from '../../state/platform';
import { useRead } from '../../state/read';
import { secretCopy } from '../../state/secretCopy';
import { tokenProblem } from '../targets/rules';
import { initialWizard, wizardReducer } from './state';
import { tokenHint } from './tokenHint';

const STEPS = ['Provider', 'Machine', 'Details'] as const;
const PROVIDER_NAMES: Readonly<Record<string, string>> = { 'hetzner-cloud': 'Hetzner Cloud' };
export const DRAFT_GONE =
  'The verified token was dropped (ten minutes passed, or the app locked). Enter it again.';

export const isDraftGone = (e: UiError) =>
  e.code === DESKTOP_ERROR_CODES.DRAFT_EXPIRED || e.code === DESKTOP_ERROR_CODES.DRAFT_NOT_FOUND;

function discardDraft(draftId: DraftId) {
  api.targetDraftDiscard(draftId).catch(reportUnlessLocked(`target_draft_discard ${draftId}`));
}

export function AddTargetWizard({ onClose }: { onClose: () => void }) {
  const info = usePlatform();
  const [state, dispatch] = useReducer(wizardReducer, undefined, initialWizard);
  const verify = useRead<TokenVerified>();
  const [notice, setNotice] = useState<string | null>(null);
  // The draft in Rust when the wizard goes: closed by hand it is discarded; a lock drops it in
  // Rust (the discard is then refused as locked, which is expected).
  const draft = useRef<DraftId | null>(null);
  draft.current = state.draft;
  useEffect(
    () => () => {
      if (draft.current !== null) discardDraft(draft.current);
    },
    [],
  );

  const forgetDraft = (message: string | null) => {
    dispatch({ type: 'forgetDraft' });
    setNotice(message);
  };
  const takeAnotherToken = () => {
    if (state.draft !== null) discardDraft(state.draft);
    verify.reset();
    forgetDraft(null);
  };

  const verifyToken = async () => {
    setNotice(null);
    const verified = await verify.run(() => api.opStartVerifyToken(state.provider, state.token));
    if (verified === null) return;
    dispatch({ type: 'verified', draft: verified.draftId });
    dispatch({ type: 'go', step: 1 });
  };

  const busy = verify.state.status === 'running';
  const step0 = (
    <>
      <ChoiceCardGroup
        legend="Provider"
        value={state.provider}
        options={SUPPORTED_PROVIDERS.map((p) => ({
          value: p,
          label: PROVIDER_NAMES[p] ?? p,
          sub: p,
        }))}
        onChange={(value) => dispatch({ type: 'provider', value })}
      />
      {state.draft === null ? (
        <PasswordField
          label="API token"
          placeholder={`${HETZNER_TOKEN_LEN} characters`}
          value={state.token}
          readOnly={busy}
          onChange={(value) => dispatch({ type: 'token', value })}
          hint={tokenHint(state.token)}
        />
      ) : (
        <div className="token-verified" role="status">
          <CheckCircleIcon aria-hidden="true" />
          <span>Token verified</span>
          <Button size={26} variant="ghost" onClick={takeAnotherToken}>
            Use another token
          </Button>
        </div>
      )}
      {notice !== null && (
        <p className="wizard-notice" role="note">
          {notice}
        </p>
      )}
      {verify.state.status === 'failed' && <ErrorPanel error={verify.state.error} />}
    </>
  );

  return (
    <Wizard
      title="Add target"
      steps={STEPS}
      step={state.step}
      hint={secretCopy(info.os, info.secretBackend)}
      onBack={() => dispatch({ type: 'go', step: 0 })}
      nextLabel={state.draft !== null ? 'Continue' : busy ? 'Verifying…' : 'Verify and continue'}
      nextDisabled={state.draft === null && tokenProblem(state.token) !== null}
      busy={busy}
      onNext={() => {
        if (state.draft !== null) dispatch({ type: 'go', step: 1 });
        else void verifyToken();
      }}
      onClose={onClose}
    >
      {state.step === 0 ? step0 : null}
    </Wizard>
  );
}
