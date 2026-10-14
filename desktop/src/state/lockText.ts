// SPDX-License-Identifier: FSL-1.1-Apache-2.0
// Every sentence the app shows about the target-store lock (D.4a surface §6, D1–D25, with the
// overview's O2 / O7 / O12 amendments): Settings › Lock, the Protect, Finish and Stop dialogs, the
// lock screen's field. One place, so each is said the same everywhere and a test reads each.
//
// Honest + separate user (owner, 2026-10-13): the lock stops an AI agent that oversteps — one
// that runs AppRafter or reads its files as you; against a program that sets out to get past it
// the lock only slows it, and a separate OS account or a sandbox is the boundary. Nothing here
// claims more, and every limit named is one some row of `apprafter lock status` or doctor shows
// (never mask). The app's own words where the CLI's are about a terminal: the app holds the key
// while it is unlocked — and after a lock only while a change the person confirmed finishes —
// and the page where the lock password is typed is a web view that runs as a process of its
// own, which no hardening of the app's process reaches (T78): the ptrace and Windows lines say
// so. Plain text: the page shows these as they are, so no `backtick` marks a command.
import type { HolderHardening } from '../ipc/generated/HolderHardening';
import type { LockSlotKind } from '../ipc/generated/LockSlotKind';
import { RECOMMENDED_PASSWORD_CHARS } from '../ipc/generated/lock';
import type { MachineLimit } from '../ipc/generated/MachineLimit';
import type { Os } from '../ipc/generated/Os';
import type { SecretsMode } from '../ipc/generated/SecretsMode';
import type { SlotRefusalView } from '../ipc/generated/SlotRefusalView';
import type { JsonValue } from '../ipc/generated/serde_json/JsonValue';

/** D1: the section's eyebrow. */
export const LOCK_EYEBROW = 'Lock';
/** D2: the sub of each row the protection pins on (T74). */
export const PINNED_SUB = 'Always on while secrets are protected: unlocking is what opens them.';
/** D3: Prefer biometrics while protected. */
export const BIOMETRICS_PROTECTED_SUB =
  'Used to confirm operations. Unlocking asks for the lock password.';
/** D11 (O7): why Auto-lock offers no Never. */
export const NEVER_PROTECTED = 'Not while secrets are protected.';
/** O7: a saved Never, while it is not offered. */
export const NEVER_SAVED_NOTE =
  'Your choice of Never comes back when protection is off; until then it is 30 min.';

/** D4: the row's label. */
export const PROTECT_LABEL = 'Protect secrets';

/** D4–D6 and the two interrupted states: where the store's secrets are. */
export function protectSub(mode: SecretsMode): string {
  switch (mode) {
    case 'files':
      return "Secrets are in files; the lock only covers this app's window.";
    case 'locking':
      return 'Protecting was interrupted. Finish it to encrypt the remaining secrets.';
    case 'locked':
      return 'Secrets are encrypted; unlocking opens them.';
    case 'unlocking':
      return 'Stopping protection was interrupted. Finish it to put the remaining secrets back in files.';
  }
}

/** What each state offers, in order: Protect, Finish (either run, interrupted), Stop protecting. */
export type ProtectAction = 'protect' | 'finish' | 'stop';
export const PROTECT_ACTIONS: Record<SecretsMode, readonly ProtectAction[]> = {
  files: ['protect'],
  locking: ['finish', 'stop'],
  locked: ['stop'],
  unlocking: ['finish'],
};
export const ACTION_LABELS: Record<ProtectAction, string> = {
  protect: 'Protect…',
  finish: 'Finish…',
  stop: 'Stop protecting…',
};

/** D7 (ADR 0068 §8, "the section says so"): why the app's unlock asks for the lock password. */
export function unlockNote(os: Os): string {
  switch (os) {
    case 'linux':
      return 'Unlocking asks for the lock password instead of your system password, which cannot open encrypted secrets yet.';
    case 'windows':
      return 'Unlocking asks for the lock password instead of Windows Hello, which cannot open encrypted secrets yet.';
    case 'macos':
      return 'Unlocking asks for the lock password instead of Touch ID or your Mac password, which cannot open encrypted secrets yet.';
  }
}

/** D8: how terminals unlock the same store (read-only in D.4a). */
export function terminalsLine(minutes: number): string {
  return minutes === 0
    ? 'Terminals: every command asks for the lock password; apprafter lock on --timeout changes it.'
    : `Terminals: each one unlocks on its own with apprafter unlock, for ${minutes} min.`;
}

/** D9: the slots, read-only in D.4a. */
export const SLOTS_LABEL = 'Opened by';
export const SLOT_NAMES: Record<LockSlotKind, string> = {
  password: 'Lock password',
  recovery_key: 'Recovery key',
  os_password: 'Account password',
  windows_hello: 'Windows Hello',
  touch_id: 'Touch ID',
  unknown: 'A kind of slot this AppRafter does not know',
};
export const SLOT_REFUSALS: Record<SlotRefusalView, string> = {
  kdf_too_weak: 'refused by this build: its key derivation is weaker than a release accepts',
  kdf_out_of_range: 'refused by this build: its key derivation parameters are out of range',
};
export const SLOTS_LATER = 'Changing them comes in a later version.';

/** One limit as the app states it: a warning or a fact. */
export interface LimitText {
  readonly warn: boolean;
  readonly text: string;
}

/** The web view's line (T78): the routes to the typed lock password the app's hardening misses. */
const TYPED = 'the web view where you type the lock password';

/** D10: `l` in the app's words; `null` for the holder base, a terminal's own matter. */
export function limitText(l: MachineLimit): LimitText | null {
  switch (l.kind) {
    case 'x11_session':
      return {
        warn: true,
        text: 'AppRafter runs as an X11 program here: other programs running as you can see what you type into it, the lock password included.',
      };
    case 'xwayland':
      return {
        warn: l.terminalIsXClient === true,
        text: 'Xwayland is running: a program that runs under it shows what you type into it to other X programs.',
      };
    case 'ptrace_scope':
      return {
        warn: true,
        text:
          l.value === null
            ? `This computer has no Yama ptrace limit: programs running as you can attach to your terminals and to ${TYPED}.`
            : `Yama ptrace_scope is ${l.value}: programs running as you can attach to your terminals and to ${TYPED}.`,
      };
    case 'windows_same_user':
      return {
        warn: true,
        text: `Programs running as you can attach themselves to an unlocked terminal, read ${TYPED}, and take over AppRafter's threads and read its memory while it holds this store's key.`,
      };
    case 'macos_task_port':
      return {
        warn: false,
        text: 'A program running as you with debugging rights can read the memory of an unlocked terminal, and of AppRafter while it holds this store’s key.',
      };
    case 'multiplexer':
      return {
        warn: false,
        text: `A terminal runs inside ${l.name}: programs running as you can type into it.`,
      };
    case 'inherited_window':
      return {
        warn: false,
        text: 'Everything started from an unlocked terminal can use its window, an AI agent included.',
      };
    case 'sleep_unreported':
      return {
        warn: true,
        text: 'This computer does not tell AppRafter when it sleeps or its screen locks. AppRafter notices a sleep from its own clocks when the computer wakes: while this target store’s secrets are protected, it then locks and drops the store’s key before any secret is used. A locked screen it does not notice.',
      };
    case 'hibernation':
      return {
        warn: false,
        text: 'Hibernation writes memory to disk, the key of an unlocked AppRafter or terminal included.',
      };
    case 'ssh_key_usable': {
      const how = l.unencrypted
        ? l.inAgent === true
          ? 'has no passphrase and is loaded in ssh-agent'
          : 'has no passphrase'
        : l.inAgent === true
          ? 'is loaded in ssh-agent'
          : 'may be loaded in ssh-agent, which did not answer within 2 s';
      return {
        warn: true,
        text: `The SSH key ${l.path} that reaches your nodes ${how}: the lock does not cover it.`,
      };
    }
    case 'runtime_dir_fallback':
      return null;
    case 'secrets_in_use':
      return {
        warn: false,
        text: 'While AppRafter or a command talks to a cluster, its kubeconfig is a file programs running as you can read.',
      };
    case 'same_user_programs':
      return {
        warn: false,
        text: 'A program running as you that sets out to get past the lock can read what you type, change what you run, or use AppRafter or a terminal once you unlock it. Run AI agents under a separate OS account or in a sandbox.',
      };
  }
}

/** O1's sentence, the app's form of `lock status`'s: what the lock covers and what it does not. */
export const AT_REST =
  "The lock protects secrets at rest. While AppRafter or a command uses one, it is in that program's memory and, for a kubeconfig, in a file programs running as you can read; the SSH key that fetches kubeconfigs stays outside the lock.";

/**
 * The app's form of `unlocked_terminal` (surface §5.8): the app holds the key while unlocked,
 * and after a lock only while a change confirmed before it finishes (the Rust verifier's
 * finding 3: the core takes a write's key before its store lock), which may first wait for
 * another AppRafter program to release the store — at most 30 s after the lock, then it is
 * abandoned unwritten (the final verifier's findings 12 and 14: a read already decrypting
 * ends, and a run keeps what it already read, so nothing here says "at once").
 */
export const APP_HOLDS_KEY =
  'While AppRafter is unlocked it holds this store’s key, and everything it runs can use the secrets: lock it when you step away. A lock stops every new read: one already decrypting ends, and a run you confirmed keeps what it already read. A change you confirmed before the lock — protecting, finishing, stopping protection, or saving, renaming or removing a token — finishes with the key it took once no other AppRafter program holds the store, or is abandoned with nothing written if it cannot start within 30 s; that key goes when the change ends.';

/**
 * T77, rule 1: what the app's own process could not do to protect the key; `null` when all took.
 * The key's page is measured only while a key is held (`holdsKey`: the report's window is the
 * app's), so nothing is said of it otherwise (critic A finding 6).
 */
export function hardeningLine(h: HolderHardening, holdsKey: boolean): string | null {
  const weak: string[] = [];
  if (h.nonDumpable === false) weak.push('other programs running as you may attach to it');
  if (h.denyAttach === false) weak.push('a debugger may attach to it');
  if (h.processDacl === false) weak.push('programs running as you may read its memory');
  if (!h.coreOff) weak.push('a crash may write its memory to disk');
  if (holdsKey && !h.memoryLocked) weak.push("the key's memory may be paged to disk");
  return weak.length === 0
    ? null
    : `AppRafter's own protections are weaker here: ${weak.join('; ')}.`;
}

/** The count Rust's `check_password` warns on (O2): Unicode scalar values, as `chars()` counts. */
export function passwordChars(password: string): number {
  return Array.from(password).length;
}

/** D13 (O2): the hint under the first field. */
export const PASSWORD_HINT = `${RECOMMENDED_PASSWORD_CHARS} characters or more is recommended. A long phrase is easier to type and harder to guess.`;

/** D13 (O2): the warning below 15, never a refusal; `null` when there is nothing to warn of. */
export function shortWarning(password: string): string | null {
  const n = passwordChars(password);
  return n > 0 && n < RECOMMENDED_PASSWORD_CHARS
    ? `Shorter than ${RECOMMENDED_PASSWORD_CHARS} characters. Anyone who copies this store's vault file, an AI agent running as you included, can try passwords against it offline.`
    : null;
}

export const PASSWORD_FIELD = 'Lock password';
export const REPEAT_FIELD = 'Repeat the lock password';
export const PASSWORDS_DIFFER = 'The two passwords differ.';
export const TYPE_A_PASSWORD = 'Type a lock password.';
export const RECOVERY_SHAPED = 'That has the shape of a recovery key. Choose another.';

/** D12 and the steps of Protect (§3.14: five, O12 split the recovery key's step). */
export const PROTECT_TITLE = 'Protect secrets';
export const PROTECT_STEPS = [
  'What it does',
  'Lock password',
  'Recovery key',
  'Confirm the key',
  'Protect',
] as const;
export function protectIntro(store: string): string {
  return `Encrypts the secrets of the target store at ${store} — its targets' Hetzner tokens and their cached kubeconfigs and Argo CD passwords — in a vault that opens only with the lock password or the recovery key. Other target stores on this computer are not changed.`;
}
/**
 * C8's app form: what the lock stops, and the boundary for what it does not. An agent started
 * inside a window the person opened uses the secrets (ADR 0068 §5, O6; critic B finding 7).
 */
export const AGENT_NOTE =
  'This stops an AI agent that runs as you from reading these secrets from files, and from using them through AppRafter or the apprafter command, except inside a window you opened: a terminal after apprafter unlock, or AppRafter once you unlock it. It does not stop a program that sets out to get past the lock — one that captures what you type: for that, run agents under a separate OS account or in a sandbox (guide: Protecting secrets from agents).';
/** G13: an earlier run wrote no vault, so its key opens nothing. */
export const RESTART_NOTE =
  'An earlier Protect stopped before it wrote the vault. The recovery key shown then is not valid; a new one follows.';
/** D14 (O12), step 3. */
export const RECOVERY_TITLE = 'Recovery key';
export const RECOVERY_BODY =
  'It opens this store if the lock password is lost, and it is shown only now. Keep it somewhere other than this computer.';
/** `<store>/vault`, with the separator the store's path itself uses (Windows' `\\`). */
export function vaultPath(store: string): string {
  const sep = store.includes('\\') && !store.includes('/') ? '\\' : '/';
  return `${store}${sep}vault`;
}
/** O8: no reset; the backup advice names the file (critic B finding 22; UI verifier minor 5). */
export function noReset(store: string): string {
  return `AppRafter cannot open a protected store without its lock password or its recovery key. The recovery key opens nothing without the vault file: include ${vaultPath(store)} in your backups; it is encrypted.`;
}
export const COPY_LABEL = 'Copy';
/**
 * O12: Rust's Copy, the same on every OS: the key is marked to stay out of clipboard history and
 * cloud clipboard sync — on macOS written for this Mac only, so Handoff does not carry it
 * (Task 17's `MacClipboard`, critic B finding 14).
 */
export const COPIED = 'Copied. The clipboard is cleared in 60 s.';
export const SAVED_IT = 'I saved it';
/** O12 (critic B finding 3): once saved, Rust took its copy off the clipboard and copies no more. */
export const COPY_GONE =
  'Taken off the clipboard when you chose I saved it, and not copied again: type it from where you saved it.';
/** D14 (O12), step 4. */
export const CONFIRM_FIELD = 'Type the whole recovery key to confirm you saved it';
export const SHOW_AGAIN = 'Show it again';
export const PROTECT_BUTTON = 'Protect';
/** D15, step 5. */
export const PROTECTED_TOAST = 'Secrets protected. Unlocking AppRafter now opens them.';
export const PROTECT_CANCELLED = 'Protecting was cancelled before any secret left its file.';
/** The run's line while it starts. */
export const STARTING = 'Starting…';

/**
 * What a Protect or Finish run reports that the page words (the core's `LockedOn`; the rest of
 * it the page does not read, and it has no TypeScript type: UI verifier M3).
 */
export interface ProtectOutcome {
  readonly alreadyLocked: boolean;
  readonly tokensLeft: readonly string[];
  readonly cachesDropped: number;
}
/** What a Stop protecting run reports that the page words (the core's `LockedOff`). */
export interface StopOutcome {
  readonly notLocked: boolean;
  readonly backedOut: boolean;
  readonly tokensKept: readonly string[];
  /** The shared age key the caches went under — `null` where none was used (d4a-f Task 3). */
  readonly ageKey: string | null;
}

const fieldsOf = (result: JsonValue): { [key in string]?: JsonValue } =>
  typeof result === 'object' && result !== null && !Array.isArray(result) ? result : {};
const namesOf = (v: JsonValue | undefined): string[] =>
  Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : [];

export function protectOutcome(result: JsonValue): ProtectOutcome {
  const f = fieldsOf(result);
  return {
    alreadyLocked: f.alreadyLocked === true,
    tokensLeft: namesOf(f.tokensLeft),
    cachesDropped: Array.isArray(f.cachesDropped) ? f.cachesDropped.length : 0,
  };
}

export function stopOutcome(result: JsonValue): StopOutcome {
  const f = fieldsOf(result);
  return {
    notLocked: f.notLocked === true,
    backedOut: f.backedOut === true,
    tokensKept: namesOf(f.tokensKept),
    ageKey: typeof f.ageKey === 'string' ? f.ageKey : null,
  };
}

/** G13: another run locked the store first; nothing this run chose was written. */
export const ANOTHER_RUN_PROTECTED =
  'Another run protected this target store first: the lock password and the recovery key you chose were not used. Unlock with the ones that run set.';

/**
 * After a Protect or Finish: what really happened — never "protected" when another run did it,
 * and a token left in clear or a cache dropped named (G2, G13; UI verifier M3).
 */
export function protectedSummary(o: ProtectOutcome, finishing: boolean): string {
  if (o.alreadyLocked) return ANOTHER_RUN_PROTECTED;
  const said = [finishing ? FINISHED_TOAST : PROTECTED_TOAST];
  if (o.tokensLeft.length > 0) {
    said.push(
      `The credentials file of ${o.tokensLeft.join(', ')} holds a token written while this ran, which is not the vault's: it stays there, unused. Renew it on its Target screen.`,
    );
  }
  if (o.cachesDropped > 0) {
    said.push(
      o.cachesDropped === 1
        ? '1 cached secret could not be re-encrypted and was removed.'
        : `${o.cachesDropped} cached secrets could not be re-encrypted and were removed.`,
    );
  }
  return said.join(' ');
}

/** A `recovery_key_invalid` refusal's fields in words; `null` for none of its reasons. */
export function recoveryKeyLine(fields: Record<string, unknown>): string | null {
  switch (fields.reason) {
    case 'mismatch':
      return 'That is not the key shown.';
    case 'checksum':
      return typeof fields.group === 'number'
        ? `That key is mistyped near group ${fields.group}.`
        : 'That key is mistyped.';
    case 'invalid_char': {
      const at = typeof fields.position === 'number' ? fields.position + 1 : null;
      const hint = typeof fields.hint === 'string' ? `: did you mean ${fields.hint}?` : '.';
      return at === null
        ? `That has a character a recovery key does not use${hint}`
        : `Character ${at} is not one a recovery key uses${hint}`;
    }
    case 'wrong_length':
      return 'That is too short or too long for a recovery key.';
    case 'wrong_prefix':
      return 'That does not start like a recovery key.';
    case 'empty':
      return 'Type the recovery key.';
    default:
      return null;
  }
}

/** The Finish dialog of an interrupted Protect (team decision 2026-10-13: both slots). */
export const FINISH_TITLE = 'Finish protecting secrets';
export function finishIntro(store: string): string {
  return `Protecting the secrets of the target store at ${store} stopped after its vault was written. Finishing opens the vault with both the lock password and the recovery key before anything more leaves the files. Without one of them, stop protecting, then protect again.`;
}
export const RECOVERY_FIELD = 'Recovery key';
export const FINISH_CONTINUE = 'Continue';
export const FINISH_BUTTON = 'Finish';
export const FINISHED_TOAST = 'Secrets protected.';

/** D16: Stop protecting (`lock off`), and its interrupted form. */
export const STOP_TITLE = 'Stop protecting secrets';
export const STOP_FINISH_TITLE = 'Finish putting secrets back in files';
/** C23. */
export const CLEAR_AGAIN =
  'The secrets will be on disk in clear again: any program running as you, an AI agent included, can read them.';
/** D16: what every Stop protecting does. */
export const STOP_BODY = 'The Hetzner tokens go back to credentials.yaml; the vault is deleted.';
/**
 * D16 and O13 in the app's form, said only where the plan uses the shared age key — to
 * re-seal a cached secret, the only use there is (d4a-f Task 3: `LockOffPayload::needs_key`).
 */
export const STOP_KEY_NOTE =
  'The cached secrets go back under the shared age key the plan names. AppRafter does not read APPRAFTER_AGE_KEY: to use another key, run apprafter lock off in a terminal.';
/**
 * The same plan where no vault is there to open — a back-out of a Protect that wrote none, the
 * finish of a Stop past the vault's deletion: nothing moves into clear (UI verifier minor 2).
 */
export const NOTHING_MOVES =
  'No vault is there to open, so no secret moves: this ends the interrupted run and leaves the secrets in their files.';
export const STOP_BUTTON = 'Stop protecting';
export const STOPPED_TOAST = 'Secrets are in files again.';
export const BACKED_OUT_TOAST = 'Backed out: the secrets are in files, as before Protect.';

/** After a Stop protecting: what really happened (UI verifier M3). */
export function stoppedSummary(o: StopOutcome): string {
  if (o.notLocked) {
    return 'This target store was not protected: another run put its secrets back in files first.';
  }
  const said = [o.backedOut ? BACKED_OUT_TOAST : STOPPED_TOAST];
  if (o.tokensKept.length > 0) {
    said.push(
      `The credentials file of ${o.tokensKept.join(', ')} held a token written while the store was protected, which differs from the vault's: that token was kept.`,
    );
  }
  // A key path only where one was used (d4a-f Task 3: no cache, no key read or created).
  if (o.ageKey !== null) said.push(`The cached secrets are under the shared age key ${o.ageKey}.`);
  return said.join(' ');
}

/** D16, D17: the store's secret, wherever it is asked. */
export const STORE_SECRET_FIELD = 'Lock password or recovery key';
/** D17: the lock screen of a protected store. */
export const STORE_FIELD_LABEL = 'Lock password';
export const STORE_FIELD_PLACEHOLDER = 'Lock password or recovery key';
export function storeLine(store: string): string {
  return `Target store ${store}`;
}
/** D19. */
export const NOT_THE_SECRET = 'That is not the lock password or the recovery key.';
/** The field is the way on a protected store; the system's own prompt opens nothing (T75). */
export const USE_THE_FIELD = 'Type the lock password or the recovery key here.';
/** D25: no system authentication, on a protected store. */
export const NO_AUTH_PROTECTED_NOTICE =
  'This computer offers no system authentication AppRafter can use: confirmations ask for the lock password.';

/** D20 and the states it did not name (critic B finding 8): the lock screen's footer. */
export function lockFooter(mode: SecretsMode, problem: boolean): string {
  if (problem)
    return 'Clusters keep running. This target store needs repair before its credentials can be used.';
  switch (mode) {
    case 'files':
      return 'Clusters keep running. Credentials are stored in files on this computer.';
    case 'locked':
      return 'Clusters keep running. Credentials stay encrypted until you unlock.';
    case 'locking':
      return 'Clusters keep running. Protecting secrets was interrupted: some credentials may still be in files.';
    case 'unlocking':
      return 'Clusters keep running. Putting secrets back in files was interrupted: some credentials may be in files again.';
  }
}
/** The Lock section's state line when the store's lock state cannot be used; the problem follows. */
export const STORE_PROBLEM_SUB = "This target store's lock state cannot be used.";
/** The lock screen's reason then, in place of "locked from a terminal" (critic A finding 22). */
export const STORE_PROBLEM_REASON = "Locked: this target store's lock state cannot be used.";
/** The Lock section's limits, and its line while lock_info is read (UI verifier minor 9). */
export const LIMITS_TITLE = 'Limits on this computer';
export const READING = 'Reading…';
