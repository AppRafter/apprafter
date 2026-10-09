// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Add target (spec §7, target add): Provider → Machine → Details, then "Save target", which is
// the Bounded plan's plain confirm (spec §4.4). The token is sent once, to verify it; Rust keeps it
// as a draft and the field is emptied (decision 6). The catalogue and the plan name the draft.
// No Paste (no clipboard read, R11), no --force, no --no-ping (decision 8), no provisioning (D.12).
import { useEffect, useReducer, useRef, useState } from 'react';
import { Button } from '../../components/Button';
import { ChoiceCardGroup } from '../../components/ChoiceCardGroup';
import { ErrorPanel } from '../../components/ErrorPanel';
import { CheckCircleIcon, SpinnerGapIcon } from '../../components/icons';
import { PasswordField } from '../../components/PasswordField';
import { StatePanel } from '../../components/StatePanel';
import { Wizard } from '../../components/Wizard';
import * as api from '../../ipc/api';
import type { DraftId } from '../../ipc/generated/DraftId';
import { DESKTOP_ERROR_CODES } from '../../ipc/generated/errors';
import type { MachineCatalogue } from '../../ipc/generated/MachineCatalogue';
import type { RegionLatency } from '../../ipc/generated/RegionLatency';
import type { TokenVerified } from '../../ipc/generated/TokenVerified';
import { HETZNER_TOKEN_LEN, SUPPORTED_PROVIDERS } from '../../ipc/generated/target';
import type { UiError } from '../../ipc/generated/UiError';
import { reportUnlessLocked } from '../../ipc/plans';
import { usePlatform } from '../../state/platform';
import { useRead } from '../../state/read';
import { secretCopy } from '../../state/secretCopy';
import { choosable, offerIn } from '../machine/catalogue';
import { MachinePicker } from '../machine/MachinePicker';
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
  const catalogueRead = useRead<MachineCatalogue>();
  const latencyRead = useRead<RegionLatency[]>();
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
  /** A new draft reads its own catalogue and latencies. */
  const resetReads = () => {
    catalogueRead.reset();
    latencyRead.reset();
  };
  const takeAnotherToken = () => {
    if (state.draft !== null) discardDraft(state.draft);
    verify.reset();
    resetReads();
    forgetDraft(null);
  };

  const verifyToken = async () => {
    setNotice(null);
    const verified = await verify.run(() => api.opStartVerifyToken(state.provider, state.token));
    if (verified === null) return;
    resetReads();
    dispatch({ type: 'verified', draft: verified.draftId });
    dispatch({ type: 'go', step: 1 });
  };

  // The catalogue of this draft, read once when the machine step shows it first.
  const catalogueIdle = catalogueRead.state.status === 'idle';
  const runCatalogue = catalogueRead.run;
  const runLatencies = latencyRead.run;
  useEffect(() => {
    if (state.step !== 1 || state.draft === null || state.catalogue !== null || !catalogueIdle) {
      return;
    }
    const draftId = state.draft;
    void runCatalogue(() => api.opStartMachineCatalogue({ kind: 'draft', draftId })).then(
      (found) => {
        if (found === null) return;
        dispatch({ type: 'catalogue', catalogue: found });
        void runLatencies(() => api.opStartRegionLatencies(found.regions.map((r) => r.code)));
      },
    );
  }, [state.step, state.draft, state.catalogue, catalogueIdle, runCatalogue, runLatencies]);

  // A draft Rust no longer has (expired, or dropped by a lock): back to the token.
  const catalogueError = catalogueRead.state.status === 'failed' ? catalogueRead.state.error : null;
  const resetCatalogue = catalogueRead.reset;
  useEffect(() => {
    if (catalogueError === null || !isDraftGone(catalogueError)) return;
    resetCatalogue();
    dispatch({ type: 'forgetDraft' });
    setNotice(DRAFT_GONE);
  }, [catalogueError, resetCatalogue]);

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

  const latencies =
    latencyRead.state.status === 'done'
      ? latencyRead.state.data
      : latencyRead.state.status === 'failed' || latencyRead.state.status === 'cancelled'
        ? [] // measured, and nothing answered: every chip shows "–"
        : null;
  const catalogueState = catalogueRead.state;
  const step1 =
    state.catalogue !== null ? (
      <MachinePicker
        catalogue={state.catalogue}
        latencies={latencies}
        region={state.region ?? ''}
        sku={state.sku}
        onRegion={(value) => dispatch({ type: 'region', value })}
        onSku={(value) => dispatch({ type: 'sku', value })}
      />
    ) : catalogueState.status === 'failed' && !isDraftGone(catalogueState.error) ? (
      <>
        <ErrorPanel error={catalogueState.error} />
        <Button onClick={catalogueRead.reset}>Try again</Button>
      </>
    ) : catalogueState.status === 'cancelled' ? (
      <StatePanel
        title="Reading the catalogue was cancelled."
        actions={<Button onClick={catalogueRead.reset}>Try again</Button>}
      />
    ) : (
      <StatePanel icon={SpinnerGapIcon} spin title="Reading the provider's catalogue…" />
    );
  const machineChosen =
    state.catalogue !== null &&
    state.region !== null &&
    choosable(offerIn(state.catalogue, state.region, state.sku));

  const next =
    state.step === 0
      ? {
          label: state.draft !== null ? 'Continue' : busy ? 'Verifying…' : 'Verify and continue',
          disabled: state.draft === null && tokenProblem(state.token) !== null,
          hint: secretCopy(info.os, info.secretBackend),
          go: () => {
            if (state.draft !== null) dispatch({ type: 'go', step: 1 });
            else void verifyToken();
          },
        }
      : {
          label: 'Continue',
          disabled: !machineChosen,
          hint: 'Prices from the provider, excl. VAT',
          go: () => dispatch({ type: 'go', step: 2 }),
        };

  return (
    <Wizard
      title="Add target"
      steps={STEPS}
      step={state.step}
      hint={next.hint}
      onBack={() => dispatch({ type: 'go', step: state.step === 2 ? 1 : 0 })}
      nextLabel={next.label}
      nextDisabled={next.disabled}
      busy={busy}
      onNext={next.go}
      onClose={onClose}
    >
      {state.step === 0 ? step0 : state.step === 1 ? step1 : null}
    </Wizard>
  );
}
