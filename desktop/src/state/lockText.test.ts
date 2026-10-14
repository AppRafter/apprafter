// SPDX-License-Identifier: FSL-1.1-Apache-2.0
import { describe, expect, test } from 'bun:test';
import type { HolderHardening } from '../ipc/generated/HolderHardening';
import type { MachineLimit } from '../ipc/generated/MachineLimit';
import type { SecretsMode } from '../ipc/generated/SecretsMode';
import * as lockText from './lockText';
import {
  AGENT_NOTE,
  hardeningLine,
  limitText,
  lockFooter,
  noReset,
  PROTECT_ACTIONS,
  passwordChars,
  protectedSummary,
  protectOutcome,
  protectSub,
  recoveryKeyLine,
  shortWarning,
  stopOutcome,
  stoppedSummary,
  terminalsLine,
  unlockNote,
} from './lockText';

/** One of every limit the core can report, in MachineLimit's order. */
const EVERY_LIMIT: readonly MachineLimit[] = [
  { kind: 'x11_session' },
  { kind: 'xwayland', terminalIsXClient: true },
  { kind: 'ptrace_scope', value: 0 },
  { kind: 'windows_same_user' },
  { kind: 'macos_task_port' },
  { kind: 'multiplexer', name: 'tmux' },
  { kind: 'inherited_window' },
  { kind: 'sleep_unreported' },
  { kind: 'hibernation' },
  { kind: 'ssh_key_usable', path: '~/.ssh/id_ed25519', unencrypted: true, inAgent: false },
  { kind: 'runtime_dir_fallback', path: '~/.config/apprafter/run' },
  { kind: 'secrets_in_use' },
  { kind: 'same_user_programs' },
];

describe('the lock password (O2)', () => {
  // The table d4a-d's Rust test (`a_short_password_is_advice_and_three_shapes_are_refused`)
  // asserts with `check_password`: an emoji is one, a combining mark one more.
  const TABLE: readonly (readonly [string, number, boolean])[] = [
    ['a'.repeat(14), 14, true],
    ['a'.repeat(15), 15, false],
    ['\u{1F600}'.repeat(14), 14, true],
    ['e\u{301}'.repeat(7), 14, true],
    [`${'e\u{301}'.repeat(7)}x`, 15, false],
    ['a', 1, true],
  ];
  test('counts Unicode scalar values as Rust does, and warns below 15', () => {
    for (const [text, chars, short] of TABLE) {
      expect({ text, chars: passwordChars(text) }).toEqual({ text, chars });
      expect({ text, short: shortWarning(text) !== null }).toEqual({ text, short });
    }
  });
  test('an empty password is no warning: the page asks for one instead', () => {
    expect(shortWarning('')).toBeNull();
  });
  test('the warning says why: an offline guess of a copied vault', () => {
    expect(shortWarning('a'.repeat(14))).toBe(
      "Shorter than 15 characters. Anyone who copies this store's vault file, an AI agent running as you included, can try passwords against it offline.",
    );
  });
});

describe("this computer's limits (rule 1)", () => {
  test('every limit but the holder base has a sentence, the warnings as the core weighs them', () => {
    const shown = EVERY_LIMIT.map((l) => [l.kind, limitText(l)?.warn ?? null] as const);
    expect(shown).toEqual([
      ['x11_session', true],
      ['xwayland', true],
      ['ptrace_scope', true],
      ['windows_same_user', true],
      ['macos_task_port', false],
      ['multiplexer', false],
      ['inherited_window', false],
      ['sleep_unreported', true],
      ['hibernation', false],
      ['ssh_key_usable', true],
      ['runtime_dir_fallback', null],
      ['secrets_in_use', false],
      ['same_user_programs', false],
    ]);
  });
  test('Xwayland warns only when the program is an X client', () => {
    expect(limitText({ kind: 'xwayland', terminalIsXClient: null })?.warn).toBe(false);
    expect(limitText({ kind: 'xwayland', terminalIsXClient: false })?.warn).toBe(false);
  });
  test('the routes to the typed lock password are named where the app has them (T78)', () => {
    for (const l of [
      { kind: 'ptrace_scope', value: 0 },
      { kind: 'ptrace_scope', value: null },
      { kind: 'windows_same_user' },
    ] as const) {
      expect(limitText(l)?.text).toContain('the web view where you type the lock password');
    }
    // While it holds the key: unlocked, and after a lock until a change you confirmed finishes
    // (the Rust verifier's finding 3) — never "while it is unlocked" alone.
    expect(limitText({ kind: 'macos_task_port' })?.text).toContain(
      'AppRafter while it holds this store’s key',
    );
    // Windows: the process DACL is the app's, its threads' are not (critic B finding 6).
    expect(limitText({ kind: 'windows_same_user' })?.text).toContain(
      "take over AppRafter's threads and read its memory while it holds this store's key",
    );
  });
  test('a computer that does not report its sleep: the app notices it on waking (O4)', () => {
    // True on every OS: the app reads the holder's clock pair on each (Task 14; UI verifier M4).
    expect(limitText({ kind: 'sleep_unreported' })?.text).toBe(
      'This computer does not tell AppRafter when it sleeps or its screen locks. AppRafter notices a sleep from its own clocks when the computer wakes: while this target store’s secrets are protected, it then locks and drops the store’s key before any secret is used. A locked screen it does not notice.',
    );
  });
  test('the SSH key line says which of the two it is, as `lock status` does', () => {
    const line = (unencrypted: boolean, inAgent: boolean | null) =>
      limitText({ kind: 'ssh_key_usable', path: '~/k', unencrypted, inAgent })?.text;
    expect(line(true, false)).toBe(
      'The SSH key ~/k that reaches your nodes has no passphrase: the lock does not cover it.',
    );
    expect(line(true, true)).toContain('has no passphrase and is loaded in ssh-agent');
    expect(line(false, true)).toContain('is loaded in ssh-agent');
    expect(line(false, null)).toContain(
      'may be loaded in ssh-agent, which did not answer within 2 s',
    );
  });
  /** Each exported function, on the arguments that reach every sentence it can say. */
  const SAMPLES: Record<string, () => unknown> = {
    protectSub: () => (['files', 'locking', 'locked', 'unlocking'] as const).map(protectSub),
    unlockNote: () => (['linux', 'windows', 'macos'] as const).map(unlockNote),
    terminalsLine: () => [terminalsLine(0), terminalsLine(5)],
    limitText: () => EVERY_LIMIT.map(limitText),
    hardeningLine: () => [
      hardeningLine(
        {
          nonDumpable: false,
          denyAttach: false,
          coreOff: false,
          memoryLocked: false,
          processDacl: false,
        },
        true,
      ),
    ],
    passwordChars: () => [],
    shortWarning: () => [shortWarning('a')],
    protectIntro: () => [lockText.protectIntro('~/s')],
    noReset: () => [noReset('~/s'), noReset('C:\\Users\\a\\apprafter')],
    vaultPath: () => [lockText.vaultPath('~/s')],
    recoveryKeyLine: () =>
      ['mismatch', 'checksum', 'invalid_char', 'wrong_length', 'wrong_prefix', 'empty'].map(
        (reason) => recoveryKeyLine({ reason, group: 1, position: 1, hint: 'l' }),
      ),
    finishIntro: () => [lockText.finishIntro('~/s')],
    storeLine: () => [lockText.storeLine('~/s')],
    lockFooter: () =>
      (['files', 'locking', 'locked', 'unlocking'] as const).flatMap((m) => [
        lockFooter(m, false),
        lockFooter(m, true),
      ]),
    protectOutcome: () => [],
    protectedSummary: () =>
      [true, false].flatMap((finishing) => [
        protectedSummary({ alreadyLocked: true, tokensLeft: [], cachesDropped: 0 }, finishing),
        protectedSummary(
          { alreadyLocked: false, tokensLeft: ['prod'], cachesDropped: 2 },
          finishing,
        ),
      ]),
    stopOutcome: () => [],
    stoppedSummary: () => [
      stoppedSummary({ notLocked: true, backedOut: false, tokensKept: [], ageKey: null }),
      stoppedSummary({ notLocked: false, backedOut: true, tokensKept: ['prod'], ageKey: '~/k' }),
      stoppedSummary({ notLocked: false, backedOut: false, tokensKept: [], ageKey: null }),
    ],
  };

  /** Every string `v` holds, at any depth. */
  const strings = (v: unknown): string[] =>
    typeof v === 'string'
      ? [v]
      : Array.isArray(v)
        ? v.flatMap(strings)
        : v !== null && typeof v === 'object'
          ? Object.values(v).flatMap(strings)
          : [];

  test('no sentence promises more than the lock does: every export, every function sampled', () => {
    // A new function joins SAMPLES, or this fails: the guard reads all of them (UI verifier
    // minor 8).
    const functions = Object.entries(lockText)
      .filter(([, v]) => typeof v === 'function')
      .map(([name]) => name)
      .sort();
    expect(functions).toEqual(Object.keys(SAMPLES).sort());
    const all = Object.entries(lockText)
      .flatMap(([name, v]) => (typeof v === 'function' ? strings(SAMPLES[name]?.()) : strings(v)))
      .join('\n');
    expect(all.length).toBeGreaterThan(5_000);
    expect(all).not.toMatch(
      /gets nothing|cannot get|secure|safe from|fully protect|always protect|without you|while it is unlocked|at once|every read/i,
    );
    // The boundary for a program that sets out to get past the lock is an OS one, said where it
    // is named.
    expect(limitText({ kind: 'same_user_programs' })?.text).toContain(
      'separate OS account or in a sandbox',
    );
    expect(AGENT_NOTE).toContain('separate OS account or in a sandbox');
    // An agent started inside a window the person opened uses the secrets (ADR 0068 §5, O6).
    expect(AGENT_NOTE).toContain('except inside a window you opened');
  });
});

describe("the app's own protections (T77, rule 1)", () => {
  const all: HolderHardening = {
    nonDumpable: true,
    denyAttach: null,
    coreOff: true,
    memoryLocked: true,
    processDacl: null,
  };
  test('nothing failed: no line', () => {
    expect(hardeningLine(all, true)).toBeNull();
  });
  test('each measure that did not take is named', () => {
    expect(hardeningLine({ ...all, nonDumpable: false }, true)).toContain(
      'other programs running as you may attach to it',
    );
    expect(hardeningLine({ ...all, denyAttach: false }, true)).toContain(
      'a debugger may attach to it',
    );
    expect(hardeningLine({ ...all, processDacl: false }, true)).toContain(
      'programs running as you may read its memory',
    );
    expect(hardeningLine({ ...all, coreOff: false }, true)).toContain(
      'a crash may write its memory to disk',
    );
    expect(hardeningLine({ ...all, memoryLocked: false }, true)).toContain(
      "the key's memory may be paged to disk",
    );
  });
  test('with no key held, nothing is said of its memory (critic A finding 6)', () => {
    expect(hardeningLine({ ...all, memoryLocked: false }, false)).toBeNull();
  });
});

describe('the state of the store', () => {
  test('each state says where the secrets are and offers its way on', () => {
    const states: readonly SecretsMode[] = ['files', 'locking', 'locked', 'unlocking'];
    expect(states.map((m) => [protectSub(m), PROTECT_ACTIONS[m]])).toEqual([
      ["Secrets are in files; the lock only covers this app's window.", ['protect']],
      [
        'Protecting was interrupted. Finish it to encrypt the remaining secrets.',
        ['finish', 'stop'],
      ],
      ['Secrets are encrypted; unlocking opens them.', ['stop']],
      [
        'Stopping protection was interrupted. Finish it to put the remaining secrets back in files.',
        ['finish'],
      ],
    ]);
  });
  test('the terminals line and the unlock note, in plain text the page shows as it is', () => {
    expect(terminalsLine(5)).toBe(
      'Terminals: each one unlocks on its own with apprafter unlock, for 5 min.',
    );
    expect(terminalsLine(0)).toContain('every command asks for the lock password');
    expect([terminalsLine(0), terminalsLine(5), AGENT_NOTE].join('')).not.toContain('`');
    expect(unlockNote('windows')).toContain('instead of Windows Hello');
    expect(unlockNote('macos')).toContain('instead of Touch ID or your Mac password');
    // D.4b's OS password slot will open it: "yet", as on the other two (UI verifier minor 15).
    expect(unlockNote('linux')).toContain(
      'instead of your system password, which cannot open encrypted secrets yet',
    );
  });
  test("the lock screen's footer says where the credentials are, interrupted runs included", () => {
    expect(lockFooter('files', false)).toBe(
      'Clusters keep running. Credentials are stored in files on this computer.',
    );
    expect(lockFooter('locked', false)).toBe(
      'Clusters keep running. Credentials stay encrypted until you unlock.',
    );
    expect(lockFooter('locking', false)).toContain('some credentials may still be in files');
    expect(lockFooter('unlocking', false)).toContain('some credentials may be in files again');
    // A lock state that cannot be used is never "encrypted" (critic B finding 8).
    for (const mode of ['files', 'locking', 'locked', 'unlocking'] as const) {
      expect(lockFooter(mode, true)).not.toContain('encrypted');
    }
  });
  test("O8: the backup advice names the vault file, with the store path's own separator", () => {
    expect(noReset('~/.config/apprafter')).toContain(
      'include ~/.config/apprafter/vault in your backups',
    );
    expect(noReset('C:\\Users\\a\\AppData\\Roaming\\apprafter')).toContain(
      'include C:\\Users\\a\\AppData\\Roaming\\apprafter\\vault in your backups',
    );
  });

  test('a run says what really happened (UI verifier M3): another run first, a token left behind', () => {
    const again = protectOutcome({ alreadyLocked: true, tokensLeft: [], cachesDropped: [] });
    expect(protectedSummary(again, false)).toBe(
      'Another run protected this target store first: the lock password and the recovery key you chose were not used. Unlock with the ones that run set.',
    );
    expect(protectedSummary(again, false)).not.toContain('Secrets protected');
    const left = protectOutcome({
      alreadyLocked: false,
      tokensLeft: ['prod'],
      cachesDropped: [{ target: 'lab', secret: 'kubeconfig', why: 'x' }],
    });
    expect(protectedSummary(left, true)).toBe(
      "Secrets protected. The credentials file of prod holds a token written while this ran, which is not the vault's: it stays there, unused. Renew it on its Target screen. 1 cached secret could not be re-encrypted and was removed.",
    );
    expect(protectedSummary(protectOutcome({}), false)).toBe(
      'Secrets protected. Unlocking AppRafter now opens them.',
    );
    expect(stoppedSummary(stopOutcome({ notLocked: true }))).toContain('was not protected');
    expect(stoppedSummary(stopOutcome({ backedOut: true }))).toBe(
      'Backed out: the secrets are in files, as before Protect.',
    );
    expect(stoppedSummary(stopOutcome({ tokensKept: ['prod'] }))).toContain('that token was kept');
    // d4a-f Task 3: a key path only where one was used — both cases of `ageKey: string | null`.
    expect(stopOutcome({ ageKey: null }).ageKey).toBeNull();
    expect(stopOutcome({}).ageKey).toBeNull();
    expect(stoppedSummary(stopOutcome({ ageKey: null }))).toBe('Secrets are in files again.');
    const keyed = stopOutcome({ ageKey: '~/.config/apprafter/age.key' });
    expect(keyed.ageKey).toBe('~/.config/apprafter/age.key');
    expect(stoppedSummary(keyed)).toBe(
      'Secrets are in files again. The cached secrets are under the shared age key ~/.config/apprafter/age.key.',
    );
  });
  test('a mistyped recovery key says what Rust found, never what was typed', () => {
    expect(recoveryKeyLine({ reason: 'mismatch' })).toBe('That is not the key shown.');
    expect(recoveryKeyLine({ reason: 'checksum', group: 7 })).toBe(
      'That key is mistyped near group 7.',
    );
    expect(recoveryKeyLine({ reason: 'checksum' })).toBe('That key is mistyped.');
    expect(recoveryKeyLine({ reason: 'invalid_char', position: 3, hint: 'l' })).toBe(
      'Character 4 is not one a recovery key uses: did you mean l?',
    );
    expect(recoveryKeyLine({ reason: 'wrong_length' })).toBe(
      'That is too short or too long for a recovery key.',
    );
    expect(recoveryKeyLine({})).toBeNull();
  });
});
