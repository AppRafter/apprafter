// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// What the Targets page and the Target screen do to a target, each by its plan class (spec §4.4,
// decision 3): `use` is reversible and runs at once, without a dialog; rename, renew and the SSH
// key are bounded — the form, then a plain confirm that lists the changes; remove is destructive
// — the full plan, the typed name, then the OS gesture inside op_execute. Every form and confirm
// is an overlay of the view it was opened from, so it hides with its tab.
import { useQueryClient } from '@tanstack/react-query';
import { useCallback, useMemo } from 'react';
import { FormDialog, type FormValues, type RadioOpt } from '../../components/FormDialog';
import { KeyIcon, PencilSimpleIcon, TrashIcon } from '../../components/icons';
import { PlanConfirm, type PlanConfirmProps } from '../../components/PlanConfirm';
import { useToast } from '../../components/Toast';
import * as api from '../../ipc/api';
import type { SshKeyCandidate } from '../../ipc/generated/SshKeyCandidate';
import type { SshKeyInfo } from '../../ipc/generated/SshKeyInfo';
import type { TargetRemoved } from '../../ipc/generated/TargetRemoved';
import type { TargetRenamed } from '../../ipc/generated/TargetRenamed';
import type { TargetRenewed } from '../../ipc/generated/TargetRenewed';
import type { TargetReport } from '../../ipc/generated/TargetReport';
import type { TargetUsed } from '../../ipc/generated/TargetUsed';
import { HETZNER_TOKEN_LEN } from '../../ipc/generated/target';
import type { UiError } from '../../ipc/generated/UiError';
import { holdPlan } from '../../ipc/heldPlans';
import { OperationFailed, resultOf, startPlan } from '../../ipc/plans';
import { useOverlay } from '../../shell/ViewFrame';
import { usePlatform } from '../../state/platform';
import { useTab } from '../../state/tab';
import { refreshTargets, targetKey } from '../../state/targets';
import { nameMessage, nameProblem, tokenMessage } from '../targets/rules';
import { removedMessage, renamedMessage, renewedMessage, usedMessage } from './outcomes';
import { keyRefusal } from './sshKey';

/** Whatever a plan or its run was refused or failed with, as the UiError the caller shows. */
const refusalOf = (reason: unknown): UiError =>
  reason instanceof OperationFailed ? reason.error : api.uiErrorOf(reason);

/** A refusal of the page's own, said in a form as a command's would be. */
const plainError = (message: string): UiError => ({
  code: null,
  message,
  help: null,
  causes: [],
  fields: {},
});

/**
 * Make a target the CLI's default: the reversible `use` plan, run at once (no confirm). A plan
 * with nothing to change (it is the default already) is discarded unrun. Either way a toast says
 * what is true now and the store is read again; a refusal or a failure goes to `onFailed`.
 */
export function useMakeDefault(
  onFailed: (error: UiError) => void,
): (name: string) => Promise<void> {
  const client = useQueryClient();
  const toast = useToast();
  return useCallback(
    async (name: string) => {
      try {
        const view = await api.opPlanTargetUse(name);
        if (view.changes.length === 0) {
          await api.opDiscard(view.opId);
          toast({ message: usedMessage({ name, pointer: null }) });
        } else {
          const out = resultOf(await (await startPlan(view.opId)).ended) as unknown as TargetUsed;
          toast({ message: usedMessage(out) });
        }
        refreshTargets(client);
      } catch (reason) {
        onFailed(refusalOf(reason));
      }
    },
    [client, toast, onFailed],
  );
}

/**
 * A bounded or destructive plan's confirm, opened as an overlay of the current view. In a tab,
 * the tab holds the plan until it runs or the confirm closes, so the tab's closing discards it
 * (heldPlans); a plan made after its tab closed is discarded at once. Also Change machine's
 * (D.3e): its own overlay, so its form is never inside the frame's (GOTCHA-144).
 */
export function useConfirm(onFailed: (error: UiError) => void) {
  const show = useOverlay();
  const { auth } = usePlatform();
  const owner = useTab()?.tab.key ?? null;
  return useCallback(
    (props: Omit<PlanConfirmProps, 'auth' | 'onFailed' | 'onClose'>) => {
      if (owner !== null) holdPlan(owner, props.view.opId);
      show((close) => <PlanConfirm {...props} auth={auth} onFailed={onFailed} onClose={close} />);
    },
    [show, auth, onFailed, owner],
  );
}

/** The radio value of "Other path…"; a key's is `key:<path>`. */
const OTHER = 'other';
const KEY = 'key:';

/**
 * A key in `~/.ssh` as a row: its `~` path, its type and comment, and whether it is in use. A
 * `.pub` the core read no public key in (no type) is shown, and cannot be chosen.
 */
function keyOption(candidate: SshKeyCandidate, inUse: boolean): RadioOpt {
  const detail = [
    candidate.algo ?? 'not an SSH public key',
    ...(candidate.comment === null ? [] : [candidate.comment]),
    ...(inUse ? ['in use now'] : []),
  ].join(' · ');
  return {
    value: `${KEY}${candidate.path}`,
    label: candidate.display,
    detail,
    disabled: inUse || candidate.algo === null,
  };
}

/**
 * The SSH key row's Change: the GUI form of `target add <name> --renew --ssh-key <path>`. A key
 * from `~/.ssh` (the one in use disabled) or another path, which the core looks up first; then the
 * bounded renew plan, with no token (the core changes the key alone and keeps the credentials),
 * and its confirm, which lists the key change. What the target uses now is read when the form
 * opens; a key that became the stored one since is the core's nothing-to-change refusal, shown
 * in the form. Also what doctor's `configure_ssh_key` fix opens (D.3e).
 */
export function useChangeSshKey(
  name: string,
  onFailed: (error: UiError) => void,
): () => Promise<void> {
  const show = useOverlay();
  const toast = useToast();
  const client = useQueryClient();
  const confirm = useConfirm(onFailed);
  return useCallback(async () => {
    let candidates: SshKeyCandidate[];
    let current: SshKeyInfo | null;
    try {
      const [found, report] = await Promise.all([api.sshKeyCandidates(), api.targetShow(name)]);
      candidates = found;
      current = report.sshKey;
    } catch (reason) {
      onFailed(refusalOf(reason));
      return;
    }
    const inUse = (path: string) => current !== null && path === current.path;
    const first = candidates.find((candidate) => !inUse(candidate.path) && candidate.algo !== null);

    /** The chosen key's path, as the core reads it; a refusal shown in the form. */
    const chosen = async (values: FormValues): Promise<string> => {
      let path: string;
      if (values.key === OTHER) {
        const info = await api.sshKeyInspect(String(values.path ?? '').trim());
        const refusal = keyRefusal(info);
        if (refusal !== null) throw plainError(refusal);
        path = info.path;
      } else {
        path = String(values.key).slice(KEY.length);
      }
      if (inUse(path)) throw plainError(`${name} uses that key now: choose another.`);
      return path;
    };

    show((close) => (
      <FormDialog
        title="Change SSH key"
        icon={KeyIcon}
        sub="The public key a new server is provisioned with. Only the key changes: the API token stays as it is."
        fields={[
          {
            key: 'key',
            kind: 'radio',
            label: 'SSH public key',
            options: [
              ...candidates.map((candidate) => keyOption(candidate, inUse(candidate.path))),
              { value: OTHER, label: 'Other path…' },
            ],
            def: first === undefined ? OTHER : `${KEY}${first.path}`,
          },
          {
            key: 'path',
            label: 'Path to a public key',
            placeholder: '~/.ssh/id_ed25519.pub',
            when: (values) => values.key === OTHER,
          },
        ]}
        required={['key', 'path']}
        submit="Continue"
        onSubmit={async (values) => {
          const path = await chosen(values);
          const view = await api.opPlanTargetRenew(name, null, path);
          confirm({
            view,
            title: `Change the SSH key of ${name}?`,
            body: 'AppRafter saves the new key path. The API token and everything else on the target stay as they are.',
            confirmLabel: 'Change key',
            icon: KeyIcon,
            onDone: (result) => {
              refreshTargets(client);
              toast({ message: renewedMessage(result as unknown as TargetRenewed), icon: KeyIcon });
            },
          });
        }}
        onClose={close}
      />
    ));
  }, [name, onFailed, show, toast, client, confirm]);
}

export interface TargetActionsOptions {
  readonly name: string;
  /** The rename ran: the tab follows the new name. */
  readonly onRenamed: (from: string, to: string) => void;
  /** The remove ran: the tab closes. */
  readonly onRemoved: (name: string) => void;
  /** A plan was refused, or its run failed or was cancelled: the screen shows it. */
  readonly onFailed: (error: UiError) => void;
}

export interface TargetActions {
  readonly rename: () => void;
  readonly renew: () => void;
  readonly remove: () => void;
  readonly makeDefault: () => void;
  readonly changeSshKey: () => void;
}

/** The Target screen's actions on `name`. A form shows its own refusals; the rest go to onFailed. */
export function useTargetActions({
  name,
  onRenamed,
  onRemoved,
  onFailed,
}: TargetActionsOptions): TargetActions {
  const show = useOverlay();
  const toast = useToast();
  const client = useQueryClient();
  const confirm = useConfirm(onFailed);
  const makeDefault = useMakeDefault(onFailed);
  const changeSshKey = useChangeSshKey(name, onFailed);

  return useMemo(() => {
    const rename = () =>
      show((close) => (
        <FormDialog
          title="Rename target"
          icon={PencilSimpleIcon}
          sub="Moves its config, credentials and local state to the new name."
          fields={[
            {
              key: 'to',
              label: 'New name',
              def: name,
              check: (value) =>
                nameMessage(nameProblem(value)) ??
                (value === name ? 'That is its name now.' : null),
            },
          ]}
          required={['to']}
          submit="Continue"
          onSubmit={async (values) => {
            const to = String(values.to);
            const view = await api.opPlanTargetRename(name, to);
            confirm({
              view,
              title: `Rename ${name} to ${to}?`,
              confirmLabel: 'Rename',
              icon: PencilSimpleIcon,
              onDone: (result) => {
                const out = result as unknown as TargetRenamed;
                // The report under its new name until the store is read again: the screen keeps
                // its cards, and the Rename button the focus returns to, instead of a spinner.
                const before = client.getQueryData<TargetReport>(targetKey(out.from));
                if (before !== undefined) {
                  client.setQueryData(targetKey(out.to), { ...before, name: out.to });
                }
                onRenamed(out.from, out.to);
                refreshTargets(client, name);
                toast({ message: renamedMessage(out), icon: PencilSimpleIcon });
              },
            });
          }}
          onClose={close}
        />
      ));

    const renew = () =>
      show((close) => (
        <FormDialog
          title="Renew API token"
          icon={KeyIcon}
          sub="Replaces only the credentials. Everything else on the target stays."
          fields={[
            {
              key: 'token',
              label: 'Hetzner Cloud token',
              type: 'password',
              placeholder: `${HETZNER_TOKEN_LEN} characters`,
              check: tokenMessage,
            },
          ]}
          required={['token']}
          submit="Continue"
          onSubmit={async (values) => {
            const view = await api.opPlanTargetRenew(name, String(values.token), null);
            confirm({
              view,
              title: `Renew the API token of ${name}?`,
              body: 'AppRafter checks the new token with the provider before it saves it.',
              confirmLabel: 'Renew',
              icon: KeyIcon,
              onDone: (result) => {
                refreshTargets(client);
                toast({
                  message: renewedMessage(result as unknown as TargetRenewed),
                  icon: KeyIcon,
                });
              },
            });
          }}
          onClose={close}
        />
      ));

    const remove = async () => {
      let view: Awaited<ReturnType<typeof api.opPlanTargetRemove>>;
      try {
        view = await api.opPlanTargetRemove(name);
      } catch (reason) {
        onFailed(refusalOf(reason));
        return;
      }
      confirm({
        view,
        title: `Remove target ${name}?`,
        icon: TrashIcon,
        body: 'Deletes its config, token and local state from this computer. Nothing changes at the provider.',
        requireText: name,
        confirmLabel: 'Remove target',
        onDone: (result) => {
          const out = result as unknown as TargetRemoved;
          onRemoved(name);
          refreshTargets(client, name);
          toast({ message: removedMessage(out), icon: TrashIcon });
        },
      });
    };

    return {
      rename,
      renew,
      remove: () => void remove(),
      makeDefault: () => void makeDefault(name),
      changeSshKey: () => void changeSshKey(),
    };
  }, [
    name,
    show,
    toast,
    client,
    confirm,
    makeDefault,
    changeSshKey,
    onRenamed,
    onRemoved,
    onFailed,
  ]);
}
