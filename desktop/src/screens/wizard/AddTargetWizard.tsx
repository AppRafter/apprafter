// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Add target (spec §7, target add): Provider → Machine → Details, then "Save target", which is
// the Bounded plan's plain confirm (spec §4.4). The token is sent once, to verify it; Rust keeps it
// as a draft and the field is emptied (decision 6). The catalogue and the plan name the draft.
// No Paste (no clipboard read, R11), no --force, no --no-ping (decision 8), no provisioning (D.12).
import { useQuery, useQueryClient } from '@tanstack/react-query';
import { useEffect, useReducer, useRef, useState } from 'react';
import { Button } from '../../components/Button';
import { ChoiceCardGroup } from '../../components/ChoiceCardGroup';
import { ErrorPanel } from '../../components/ErrorPanel';
import { CheckCircleIcon, HardDrivesIcon, SpinnerGapIcon } from '../../components/icons';
import { PasswordField } from '../../components/PasswordField';
import { PlanConfirm } from '../../components/PlanConfirm';
import { RadioList, type RadioListItem } from '../../components/RadioList';
import { StatePanel } from '../../components/StatePanel';
import { TextField } from '../../components/TextField';
import { useToast } from '../../components/Toast';
import { Wizard } from '../../components/Wizard';
import * as api from '../../ipc/api';
import { uiErrorOf } from '../../ipc/api';
import { CORE_ERROR_CODES } from '../../ipc/generated/core-errors';
import type { DraftId } from '../../ipc/generated/DraftId';
import { DESKTOP_ERROR_CODES } from '../../ipc/generated/errors';
import type { MachineCatalogue } from '../../ipc/generated/MachineCatalogue';
import type { PlanView } from '../../ipc/generated/PlanView';
import type { RegionLatency } from '../../ipc/generated/RegionLatency';
import type { TargetAdded } from '../../ipc/generated/TargetAdded';
import type { TokenVerified } from '../../ipc/generated/TokenVerified';
import { HETZNER_TOKEN_LEN, SUPPORTED_PROVIDERS, TIERS } from '../../ipc/generated/target';
import type { UiError } from '../../ipc/generated/UiError';
import { failureOf, isCancelled, reportUnlessLocked, runPlan } from '../../ipc/plans';
import { usePlatform } from '../../state/platform';
import { useRead } from '../../state/read';
import { secretCopy } from '../../state/secretCopy';
import { refreshTargets, TARGETS_KEY } from '../../state/targets';
import { choosable, offerIn } from '../machine/catalogue';
import { MachinePicker } from '../machine/MachinePicker';
import { providerLabel, tierLabel } from '../targets/labels';
import { nameMessage, nameProblem, tokenProblem } from '../targets/rules';
import { addArgs, addedMessage, initialWizard, type SshChoice, wizardReducer } from './state';
import { tokenHint } from './tokenHint';

const STEPS = ['Provider', 'Machine', 'Details'] as const;
export const DRAFT_GONE =
  'The verified token was dropped (ten minutes passed, or the app locked). Enter it again.';
const TOKEN_USED = 'The token was used by the attempt that failed. Enter it again to retry.';
const SAVE_CANCELLED = 'Saving was cancelled; nothing was saved.';
const NAME_HINT = 'Letters, digits and dashes.';

export const isDraftGone = (e: UiError) =>
  e.code === DESKTOP_ERROR_CODES.DRAFT_EXPIRED || e.code === DESKTOP_ERROR_CODES.DRAFT_NOT_FOUND;

const plainError = (message: string): UiError => ({
  code: null,
  message,
  help: null,
  causes: [],
  fields: {},
});

function discardDraft(draftId: DraftId) {
  api.targetDraftDiscard(draftId).catch(reportUnlessLocked(`target_draft_discard ${draftId}`));
}

const sshValue = (ssh: SshChoice | null): string | null =>
  ssh === null ? null : ssh.kind === 'key' ? `key:${ssh.path}` : ssh.kind;

interface PathStatus {
  readonly text: string;
  readonly tone: 'ok' | 'err' | 'faint';
}

/** The Other path row's field: the path is checked by the core when the field is left. */
function OtherPath({
  text,
  status,
  onText,
  onLeave,
}: {
  readonly text: string;
  readonly status: PathStatus | null;
  readonly onText: (text: string) => void;
  readonly onLeave: () => void;
}) {
  return (
    <TextField
      label="Path to a public key"
      value={text}
      placeholder="~/.ssh/id_ed25519.pub"
      onChange={onText}
      onBlur={onLeave}
      hint={
        status === null ? (
          'Checked when you leave the field.'
        ) : (
          <span className="ssh-path-status" data-tone={status.tone}>
            {status.text}
          </span>
        )
      }
    />
  );
}

export function AddTargetWizard({ onClose }: { onClose: () => void }) {
  const info = usePlatform();
  const client = useQueryClient();
  const toast = useToast();
  const [state, dispatch] = useReducer(wizardReducer, undefined, initialWizard);
  const verify = useRead<TokenVerified>();
  const catalogueRead = useRead<MachineCatalogue>();
  const latencyRead = useRead<RegionLatency[]>();
  const [notice, setNotice] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<UiError | null>(null);
  const [confirm, setConfirm] = useState<PlanView | null>(null);
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
    // A verify that completes after the wizard went (or after a reset) left a draft in Rust that
    // nothing will use: it goes at once.
    const verified = await verify.run(
      () => api.opStartVerifyToken(state.provider, state.token),
      (unused) => discardDraft(unused.draftId),
    );
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

  // The details step: the store's names (a taken one is said inline), the keys under ~/.ssh,
  // and the core's look at a path typed under Other.
  const details = state.step === 2;
  const targets = useQuery({ queryKey: TARGETS_KEY, queryFn: api.targetList, enabled: details });
  const candidates = useQuery({
    queryKey: ['ssh-key-candidates'],
    queryFn: api.sshKeyCandidates,
    enabled: details,
  });
  const checkedPath = state.ssh?.kind === 'other' ? state.ssh.checked : null;
  const inspected = useQuery({
    queryKey: ['ssh-key', checkedPath],
    queryFn: () => api.sshKeyInspect(checkedPath ?? ''),
    enabled: checkedPath !== null && checkedPath !== '',
  });

  // The first key found is the default, as the CLI's picker's first row is.
  const firstKey = candidates.data?.[0]?.path;
  useEffect(() => {
    if (state.ssh === null && firstKey !== undefined) {
      dispatch({ type: 'ssh', value: { kind: 'key', path: firstKey } });
    }
  }, [state.ssh, firstKey]);

  const problem = nameProblem(state.name);
  const taken =
    targets.data?.targets.some((t) => t.name === state.name) === true ||
    targets.data?.unreadable.some((t) => t.name === state.name) === true;
  const ssh = state.ssh;
  // The Other path counts once the core has looked at what the field holds now.
  const otherChecked =
    ssh?.kind === 'other' && ssh.checked !== null && ssh.checked !== ''
      ? ssh.checked === ssh.text.trim()
      : false;
  // undefined: no usable choice yet; null: Skip.
  const sshKey: string | null | undefined =
    ssh === null
      ? undefined
      : ssh.kind === 'key'
        ? ssh.path
        : ssh.kind === 'skip'
          ? null
          : otherChecked && inspected.data?.exists === true
            ? inspected.data.path
            : undefined;
  const args = sshKey === undefined || problem !== null || taken ? null : addArgs(state, sshKey);

  const pathStatus: PathStatus | null = !otherChecked
    ? null
    : inspected.isPending
      ? { text: 'Checking…', tone: 'faint' }
      : inspected.isError
        ? { text: uiErrorOf(inspected.error).message, tone: 'err' }
        : inspected.data?.exists === true
          ? { text: `Found · ${inspected.data.algo ?? 'unknown type'}`, tone: 'ok' }
          : { text: 'No file at that path.', tone: 'err' };

  const finishAdded = (added: TargetAdded) => {
    refreshTargets(client); // D.3d's: the Targets page and list read again
    toast({ message: addedMessage(added), icon: HardDrivesIcon });
    onClose();
  };

  // The Save click confirmed it: run it here and show a failure inline.
  const runPlanned = async (plan: PlanView) => {
    setSaving(true);
    try {
      finishAdded(await runPlan<TargetAdded>(plan.opId));
    } catch (reason) {
      setSaving(false);
      setSaveError(isCancelled(reason) ? plainError(SAVE_CANCELLED) : failureOf(reason));
    }
  };

  const save = async () => {
    if (args === null) return;
    setSaving(true);
    setSaveError(null);
    let plan: PlanView;
    try {
      plan = await api.opPlanTargetAdd(args);
    } catch (reason) {
      setSaving(false);
      const e = uiErrorOf(reason);
      if (isDraftGone(e) || e.code === CORE_ERROR_CODES.TARGET_INVALID_TOKEN) {
        // A refused plan leaves a live draft in Rust; one that cannot be used goes now.
        discardDraft(args.draftId);
        forgetDraft(isDraftGone(e) ? DRAFT_GONE : e.message);
      } else {
        setSaveError(e);
      }
      return;
    }
    dispatch({ type: 'draftTaken' }); // the plan holds the token now
    if (plan.class === 'destructive') {
      setSaving(false);
      setConfirm(plan); // its dialog first
    } else {
      await runPlanned(plan); // Bounded: the Save click was its plain confirm
    }
  };

  // The draft went with a plan whose run failed: a new token makes a new draft, whose verify
  // reads its own catalogue.
  const enterTokenAgain = () => {
    setSaveError(null);
    forgetDraft(TOKEN_USED);
  };

  const chooseSsh = (value: string) => {
    if (value === 'skip') dispatch({ type: 'ssh', value: { kind: 'skip' } });
    else if (value === 'other') {
      dispatch({
        type: 'ssh',
        value: ssh?.kind === 'other' ? ssh : { kind: 'other', text: '', checked: null },
      });
    } else dispatch({ type: 'ssh', value: { kind: 'key', path: value.slice('key:'.length) } });
  };
  const otherText = ssh?.kind === 'other' ? ssh.text : '';
  const sshItems: RadioListItem<string>[] = [
    ...(candidates.data ?? []).map((k) => ({
      value: `key:${k.path}`,
      ariaLabel: k.display,
      label: k.display,
      detail: k.algo ?? 'unknown type',
      ...(k.comment !== null && { meta: k.comment }),
    })),
    {
      value: 'other',
      ariaLabel: 'Other path…',
      label: 'Other path…',
      expanded: (
        <OtherPath
          text={otherText}
          status={pathStatus}
          onText={(text) =>
            dispatch({ type: 'ssh', value: { kind: 'other', text, checked: null } })
          }
          onLeave={() =>
            dispatch({
              type: 'ssh',
              value: { kind: 'other', text: otherText, checked: otherText.trim() },
            })
          }
        />
      ),
    },
    {
      value: 'skip',
      ariaLabel: 'Skip',
      label: 'Skip',
      detail: 'No key: provisioning refuses until the target has one.',
    },
  ];

  const verifying = verify.state.status === 'running';
  const busy = verifying || saving;
  const step0 = (
    <>
      <ChoiceCardGroup
        legend="Provider"
        value={state.provider}
        options={SUPPORTED_PROVIDERS.map((p) => ({ value: p, label: providerLabel(p), sub: p }))}
        onChange={(value) => dispatch({ type: 'provider', value })}
      />
      {state.draft === null ? (
        <PasswordField
          label="API token"
          placeholder={`${HETZNER_TOKEN_LEN} characters`}
          value={state.token}
          readOnly={verifying}
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

  const nameHint =
    state.name === ''
      ? NAME_HINT
      : problem !== null
        ? (nameMessage(problem) ?? '')
        : taken
          ? `A target named ${state.name} exists.`
          : NAME_HINT;
  const step2 = (
    <div className="wizard-details">
      <TextField
        label="Target name"
        value={state.name}
        placeholder="prod-us"
        aria-invalid={(state.name !== '' && problem !== null) || taken || undefined}
        hint={nameHint}
        onChange={(value) => dispatch({ type: 'name', value })}
      />
      <ChoiceCardGroup
        legend="Default tier"
        columns={4}
        value={state.tier}
        options={TIERS.map((t) => ({ value: t.id, label: tierLabel(t.id, t.level) }))}
        hint="A hint for later commands; the machine you chose is what gets provisioned: one server."
        onChange={(value) => dispatch({ type: 'tier', value })}
      />
      <RadioList
        legend="SSH public key"
        value={sshValue(ssh)}
        items={sshItems}
        onChange={chooseSsh}
      />
      {candidates.isError && <ErrorPanel error={uiErrorOf(candidates.error)} />}
      {saveError !== null && <ErrorPanel error={saveError} />}
    </div>
  );

  const next =
    state.step === 0
      ? {
          label:
            state.draft !== null ? 'Continue' : verifying ? 'Verifying…' : 'Verify and continue',
          disabled: state.draft === null && tokenProblem(state.token) !== null,
          hint: secretCopy(info.os, info.secretBackend),
          go: () => {
            if (state.draft !== null) dispatch({ type: 'go', step: 1 });
            else void verifyToken();
          },
        }
      : state.step === 1
        ? {
            label: 'Continue',
            disabled: !machineChosen,
            hint: 'Prices from the provider, excl. VAT',
            go: () => dispatch({ type: 'go', step: 2 }),
          }
        : {
            // A failed or cancelled run leaves no draft (the plan took it): the button turns.
            label: saving
              ? 'Saving…'
              : state.draft === null
                ? 'Enter the token again'
                : 'Save target',
            disabled: state.draft !== null && args === null,
            hint: 'Name: letters, digits and dashes',
            go: () => {
              if (state.draft === null) enterTokenAgain();
              else void save();
            },
          };

  return (
    <>
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
        {state.step === 0 ? step0 : state.step === 1 ? step1 : step2}
      </Wizard>
      {/* Beside the wizard, not in it: the wizard is a form, and the confirm's own form would
          sit inside it, its submit bubbling up to the wizard's Next. */}
      {confirm !== null && (
        <PlanConfirm
          view={confirm}
          title={confirm.title}
          confirmLabel="Save target"
          auth={info.auth}
          body="Read what this plan changes before it runs."
          onDone={(result) => finishAdded(result as TargetAdded)}
          onFailed={(e) =>
            setSaveError(e.code === CORE_ERROR_CODES.OP_CANCELLED ? plainError(SAVE_CANCELLED) : e)
          }
          onClose={() => setConfirm(null)}
        />
      )}
    </>
  );
}
