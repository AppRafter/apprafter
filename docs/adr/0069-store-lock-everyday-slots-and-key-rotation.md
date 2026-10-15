# ADR 0069: Store-lock everyday slots, slot management and key rotation

## Status

`Proposed`.

Date: 2026-10-15. ADR for `plan.md` Phase D (sub-phase D.4b, the target-store lock's second stage).
A follow-up of [ADR 0068](0068-target-store-lock.md), whose Owner section sends later amendments
through a follow-up record: this record does not edit ADR 0068's text, and §11 lists what it
changes there. The owner confirms or corrects the readings of §2 when he accepts it. The
decisions are expected to be revisited after their first real use (Re-evaluation).

## Context

ADR 0068 introduced an opt-in lock for a target store: its secrets in an encrypted vault, opened
through key slots, and a per-terminal unlock window. Its first stage shipped the lock-password and
recovery-key slots. Its second stage adds the slots a person uses every day and the commands that
change how a store opens: the account-password slot (Linux, macOS), the Windows Hello slot,
`lock slot add | remove | list`, `lock rekey` and `lock recovery`.

ADR 0068 left the mechanism of each such slot open, and its words leave room for more than one
design. Its table says these commands are confirmed with "a slot", which can mean any slot or only
the lock password and the recovery key. It says nothing of whether a key change also replaces the
store's own age key, which encrypts every cached kubeconfig and Argo CD password, or of how a slot
behaves after the account password changes.

Before the design was fixed, nine questions went to the owner, each with options, a
recommendation and the cost of each option. The answers below are his. Four of them depart from
the recommendation. Each is recorded with the cost it carries, in the words the product will use
to state it.

Each design decision below that rests on platform behaviour was checked first against throwaway
accounts the checks created, in containers and on CI runners, never on a person's account. The
Appendix lists what those checks found.

## Decision

### 1. The owner's answers

| # | Decision | Departs from the recommendation | Cost, as it is stated everywhere |
|---|---|---|---|
| 1 | **The account-password slot is bound to this computer.** Its key also takes this computer's machine id (Linux `/etc/machine-id`, macOS the hardware UUID), which a copy of the home folder or of the store does not carry; a copied vault alone tests no guess. | No | The slot stops opening after a Linux reinstall (a new machine id; a Mac's hardware UUID survives a reinstall and changes with the Mac), on another computer, and after the machine id changes; then the recovery key, or a lock password if one was added, opens the store, and the slot is added again. A program running as the person reads the machine id too and can guess the password offline. Cloned machines and images share an id unless it is regenerated. A full-disk backup of a Linux machine contains it. |
| 2 | **The account password is checked with the system when its slot is added** (when `lock slot add os-password` runs, and at the default `lock on`): one try per run, a back-off across runs. Where no check sees the typed text, there is no account-password slot, and the refusal says why. | No | A wrong password may count as a failed login (and lock the account on systems that lock after failures). A typo at `lock on` costs the back-off's wait, or `--lock-password`. On a computer without such a check there is no account-password way in. |
| 3 | **One everyday way in, by default.** The owner's words: there must be a default in which one password or Windows Hello does everything; a separate password is worth building, but it is certainly not the default path. `lock on` and the desktop's Protect set up one everyday way in — the account password (Linux, macOS), checked with the system and bound to this computer, or Windows Hello — and that way does everything: it opens the store and changes how it opens. The recovery key is always made. A separate lock password is an option (`lock on --lock-password`, a choice in Protect) and the automatic fallback where this computer has no usable account password or Windows Hello. | Yes: the recommendation kept the lock password as the default and offered the everyday way as a second step | The everyday secret of a default store is the account password, which can be guessed offline from a copy of the vault together with the machine id and is known to anyone given it for `sudo`, or one Windows Hello approval, whose dialog does not say which program asks. That one secret also changes how the store opens (answer 8). On another computer or after a reinstall, a default store opens only with the recovery key. |
| 4 | **A key change keeps what is proven in that run.** The secret typed to open the store is kept; the recovery key is kept if it is typed, else a new one is made; Windows Hello and the account password are kept if confirmed, else dropped; a typed slot is dropped only when the person types `drop`; the plan lists every slot's fate before anything is asked. | No | Typing the recovery key to keep it exposes it like any typed secret. Enter at that question means a new recovery key on paper. |
| 5 | **Windows Hello wherever the typed prompt would appear.** The terminal first prints which command asks and which program started it; cancelling Hello falls back to typing; a Hello failure says why. | No | A program running as the person can raise the same dialog and hope for a reflex approval, and with answer 8 that approval changes how the store opens. Stated in the slot's note, on the terminal line and in the documentation. |
| 6 | **Agents never check an account password or ask Windows Hello on a person's computers**, only in a VM, container or CI runner whose test account they made, and every AppRafter command an agent runs sets `APPRAFTER_SANDBOX`. This is now a rule of the project's agent instructions, and the agents' own run scripts follow it. | No | None beyond the rule. |
| 7 | **The macOS and Windows checks by the owner do not block the Linux work.** A Windows Hello probe script is delivered first and only the Windows Hello steps wait for its answers; everything else proceeds and is walked on Linux, in VMs and containers with test accounts; the macOS account-password rows run on a throwaway macOS account. | Partly: no early walk on the owner's machines | The real macOS check route and the real Windows Hello behaviour meet a shipped build only in the owner's final walk; a CI job against an account it creates proves the macOS route first, and the probe proves Hello's. |
| 8 | **Any slot may change how the store opens**: `lock slot add`, `lock slot remove`, `lock rekey`, `lock recovery`, `lock off`, and the unlock window's length (`lock on --timeout` on a locked store). Every vault write still asks a fresh secret, of any slot now, even inside an unlock window. | Yes: the recommendation was to let only the lock password or the recovery key change how a store opens | **An agent that knows the account password — given it for `sudo`, or that saw it typed — or that gets one reflexive Windows Hello approval can take over the store's management**: set a lock password of its own, make a recovery key only it has seen, remove the person's ways in (typing `drop` itself), or run `lock off`. The person is then locked out of their own store, or finds its secrets in clear files. The mitigation is the documentation's advice: agents under a separate OS account or in a sandbox, and `--lock-password` on a machine where agents get the account password. |
| 9 | **Every key change replaces the store's own age key** (`lock rekey`, `lock recovery`) and re-encrypts every target's cached secrets under the new one, as a resumable, crash-safe operation over many files, with tests and a walk row. | Yes: the recommendation was to keep the store's age key and state what stays exposed | Every password change re-encrypts every cache (seconds per target). An interrupted key change refuses every read until `lock rekey` or `lock recovery` finishes it, or `lock off` returns the secrets to files. A release of the lock's first stage refuses such a store until then. |

### 2. Readings to confirm at acceptance

The owner's answers met each other and ADR 0068 in five places where a reading had to be chosen.
The owner confirms or corrects each of them when he accepts this record:

1. **Windows Hello at every prompt** (answer 5 with answers 3 and 8). Answer 5 names reads and
   `unlock`. Since any slot changes how a store opens, and a Windows store's one everyday way in is
   Hello, Hello is offered at every prompt for the store, management included; the terminal line
   names the command, its program and, for management, what it changes; the desktop's
   confirmations get a Hello button.
2. **`lock rekey` replaces the lock password** (answer 4 with ADR 0068 §5, where rekey "changes
   the password"). Where a store has a lock password, `lock rekey` sets a new one; every other way
   in is kept as answer 4 says; on a store without a lock password (the new default) rekey is a
   pure key change. Reading 4 below supersedes this reading's narrower view of `drop`.
3. **The agent rule needs no new wording** (answer 6 with answer 3). The rule's first clause, that
   an agent never checks an account password on a person's computer, already covers a plain
   `lock on`, which now checks the account password by default.
4. **`drop` for every typed way in** (answer 4 with answer 3). At a key change's question about a
   typed way in — the lock password in `lock recovery`, and this computer's account password —
   Enter does not drop it: the question repeats, and only typing `drop` drops it. The recovery key:
   Enter makes a new one (answer 4). Windows Hello: cancelling drops it, except when it is the last
   way in besides the recovery key, where the person is asked to type `drop`. So a default store
   never ends with only the paper key after a stray Enter.
5. **An empty answer means "no password"** (answer 3's fallback where the system does not tell).
   Where AppRafter can read that this account has no usable password, `lock on` falls back to a lock
   password by itself. Where it cannot, the account-password prompt says "press Enter on an empty
   line if this account has no password", and an empty entry falls back before any check — the one
   fallback that rests on the person's answer.

### 3. The account-password slot (Linux, macOS)

- **Slot kind `0x03`**, reserved by ADR 0068, gets a body. The vault format stays version 1:
  readers that predate this record keep the slot byte for byte and never open it; a reader of this
  record keeps a body it cannot read (an unknown layout byte, a wrong length) and refuses only that
  slot.
- **Its key** comes from Argon2id over the password as typed, under the same policy as a lock
  password, combined with a secret derived from this computer's machine id. Its exact bytes: on
  Linux the 32 lowercase hexadecimal characters of `/etc/machine-id` without the line end (any other
  content means no machine id); on macOS the 16 raw bytes `gethostuuid()` returns. A per-slot tag
  says "made on this computer" without revealing the id and without linking two stores.
- **One slot per computer.** Adding the account password where this computer already has a slot
  replaces it in the same write (the re-enrolment after a password change); another computer's
  slot stays.
- **Checked when added, never when used.** The check is one PAM call (Linux; libpam loaded at run
  time, so the CLI starts where libpam is absent) or, on macOS, the route the Appendix records. It
  counts as a check only when the stack asked for the password exactly once and the answer was the
  typed text; a stack that succeeds without asking (a fingerprint reader, `pam_permit`) or asks
  something else is refused as "not checked". Opening the slot never calls PAM: its key check
  decides, so no unlock counts as a failed login.
- **After the account password changes**, the slot still opens with the previous password and not
  with the new one; AppRafter says so after a failed attempt, the previous password may authorise
  adding the new one (answer 8), and `doctor` warns where the system tells the change time.

### 4. The Windows Hello slot

- Slot kind `0x04` gets a body when the mechanism is proven on real devices. The rule for choosing
  it is fixed here; the mechanism it picks is recorded in the implementation plan, the
  how-it-works page and the guide, not by editing this record:
  - the **signature route** (`KeyCredentialManager`: the key material is the Hello key's signature
    over a per-slot challenge that includes the vault id) when signatures are deterministic
    RSASSA-PKCS1-v1_5 over SHA-256 and every signature asks a fresh gesture;
  - else the **decrypt route** (a Hello-gated key of the Microsoft Passport key storage provider
    that decrypts a wrapped secret), if it works for an unpackaged program;
  - else no Windows Hello slot in this stage, and Windows' default stays a lock password, said.
- The signature is verified before it is used; a key not kept in a TPM is allowed only after the
  plan says what that costs; weak keys are refused. A Hello key stays on the computer that made it.

### 5. `lock on`'s default and its fallback

- Without `--lock-password`, `lock on` sets up this computer's everyday way: Windows Hello on
  Windows, the account password on Linux and macOS, plus the recovery key. With it, `lock on` is the
  first stage's flow, byte for byte.
- It falls back to a lock password **on facts only**, checked in this order and named in the plan's
  first lines: the agent fence is in force; this build has no Windows Hello yet; the system has no
  usable check (no libpam, no PAM service, no proven macOS route; Hello not supported or not set
  up); a remote session; no machine id; no place for the back-off's record; no password on this
  account (a fact where readable, else the empty answer of reading 5).
- A wrong password, a back-off in force, a stack that did not check the typed text, a declined
  software Hello key and a cancelled Hello prompt **end** the run: nothing is written and no
  recovery key is shown. They never fall back.
- Every new recovery key is shown with its slot's short id, after the words "this key opens the
  store only when AppRafter says Done", so a key shown before a crash is never mistaken for a valid
  one.

### 6. Who can change how a store opens

Any slot's fresh secret, typed or a Windows Hello approval, opens a command that changes how the
store opens (answer 8). Never an unlock window's holder, and never the key the unlocked desktop
holds: an agent started inside a person's window, without a secret of its own, changes nothing.
Every surface where an everyday slot is set up or listed says what this costs (answer 8's cost).

### 7. Key changes and the `rotating` state

- `lock rekey` and `lock recovery` replace the vault's data key, its vault id and the store's own
  age key, keep the store id, and keep each proven slot under its own slot id (answer 4).
- `store.yaml` gains the state `rotating`, with the new vault id and the Windows Hello keys to
  delete; during a key change the vault carries the retiring age key until every cache is
  re-encrypted and durable. Readers refuse a `rotating` store; `lock rekey` or `lock recovery`
  finishes it with one fresh secret; `lock off` from `rotating` opens each cache with either key and
  returns the secrets to files, so ADR 0068's "a person can always return to files" holds.
- A key change that fails before its new vault is durable backs out to `locked` by itself. A key
  change another process is running is told from an interrupted one by the store's write lock.
- No cache is ever sealed only to a key that no vault on disk holds. A state file that does not
  parse is renamed aside and named, never deleted, and its secrets are fetched again.
- What a key change does not do is said in its plan and outcome: copies of the vault made before it
  (a backup, a snapshot) still open with the old secrets and hold the old store key; the Hetzner
  tokens and cluster credentials themselves are unchanged (renew them at their source); restoring an
  older backup brings back the old vault.

### 8. Every writer of a target's state file under the store lock

`apply`, `destroy`, `import` and `init` re-read, merge and save a target's state file under the
store's write lock, taking the caches only from that re-read, so no command can bring back a cache
under a store key a key change retired. This lands before the key change does.

### 9. One OS crate, one fence

- The account-password check, the machine id, the back-off and the Windows Hello calls live in one
  crate the CLI and the desktop share; the core never calls the OS. The CLI gains a run-time-loaded
  PAM and, with the Hello slot, WinRT.
- A real backend can be made only through that crate's fence: with `APPRAFTER_SANDBOX` set, a PAM
  check runs only for an account whose home lies inside a sandbox root and carries a test-account
  marker, and every Windows Hello call is refused. Both clients pass the variable in every build.
  The variable can only add refusals.
- Touch ID stays staged until the CLI is signed with a Developer ID; slot kind `0x05` stays
  reserved, and a vault holding one is shown as made by a newer AppRafter.

### 10. Slot management

`lock slot list` shows every slot with its short id, label, notes and any refusal, and asks nothing.
`lock slot add password | os-password | windows-hello` adds one way in; `lock slot remove` removes
one, never the recovery key, and never changes the keys — its plan says that copies made before
still open with the removed secret and that `lock rekey` changes the keys themselves. The desktop's
Security page gains the same actions under **Opened by**.

### 11. What this record changes in ADR 0068

- **§1, qualified.** The guarantee against an agent that oversteps holds only while the agent
  knows no secret that opens the store. By default that secret is now the person's account
  password, and any secret also changes how the store opens: an agent given the account password
  for `sudo`, or one reflexive Windows Hello approval, takes over the store's management and can
  lock the person out (Risks). The documentation's OS-boundary advice is the mitigation, and
  `--lock-password` on a machine where agents get the account password.
- **§2, extended.** `store.yaml`'s states gain `rotating`, with the fields only that state has; the
  vault's payload may carry the retiring age key during a key change. A release of the lock's first
  stage refuses a `rotating` store: it reads the unknown state as an unreadable file and uses none
  of its secrets.
- **§3, specified.** The account-password slot is checked with the system when added, bound to
  this computer, one per computer, and absent where no check sees the typed text; the Windows Hello
  slot follows §4's rule; `lock on` sets up one everyday way by default, with the lock password as
  an option and a fallback and the recovery key always. "Every store keeps at least one
  device-independent slot" holds through the recovery key.
- **§5, amended.** `lock on`'s confirmation is the everyday way's secret or gesture (a lock
  password twice under `--lock-password`) and the recovery key typed back. "A slot" in the D.4b row
  means any slot's fresh secret. `lock rekey` and `lock recovery` are key changes that keep what is
  proven and replace the store's age key, resumable; `lock off` reaches files from `rotating`;
  `lock slot remove` never changes the keys. The open plan classes: `lock slot remove` is
  destructive, `lock rekey` and `lock recovery` are bounded, each with a fresh secret.
- **Risks, "Capturing the typed secret", narrowed.** The gesture slots remove the typed-secret route
  for the people who use Windows Hello. The account password is a typed secret, typed in more
  places than a lock password (`sudo`, the screen lock, polkit dialogs), so it widens that route.
- **Risks, "Gesture slots that yield keys" and "An OS-password slot after a password change"**,
  settled by §3 and §4.

## Consequences

- A store protected by default needs no new secret to remember: the account password or Windows
  Hello opens it, and the recovery key is the way back.
- A lock password remains for people who want a secret only AppRafter's prompt sees, and becomes
  the default where the computer has no usable account password or Windows Hello.
- Changing how a store opens is as easy as opening it, for the person and for anyone who holds the
  same secret. That is the owner's choice, and the cost is stated on every surface.
- A password change ends with a key change that re-encrypts every cache and ends every unlock
  window and unlocked desktop of the store.
- The CLI loads libpam at run time and, with the Windows Hello slot, links WinRT; neither is needed
  to start it.
- Stores locked by a release of the lock's first stage open and are managed as before. Such a
  release opens a store made by the new default only with the recovery key, and refuses a
  `rotating` store.

## Alternatives considered

- **Management only with the lock password or the recovery key.** It keeps a device-independent
  slot the person's own even when an agent knows the account password. Not chosen (answer 8): a
  default whose one everyday way cannot manage the store would send people to the paper key for
  every routine change.
- **The lock password as the default, the everyday way offered afterwards.** It keeps the first
  stage's flow and makes no offline-guessable slot the only everyday way. Not chosen (answer 3).
- **An account-password slot not bound to the computer, or none at all.** Not chosen (answer 1):
  binding closes the route through backups and synced copies of the home folder at a small cost.
- **Keeping the store's age key through a key change**, with a two-command reset (`lock off`, then
  `lock on`) for a new one. Not chosen (answer 9): an old secret with an old vault copy would keep
  decrypting caches written after the change.
- **A recovery key as a public-key slot**, re-wrapped by every key change without being typed. Not
  chosen (answer 4): it adds a slot kind, a conversion, and a store that releases of the lock's
  first stage open only with the lock password.
- **An account-password slot labelled "not checked"** where no check sees the typed text. Not
  chosen (answer 2): two kinds of slot under one name.

## Risks

- **One account password or one Hello approval takes over a store's management** (answer 8).
  An agent that knows the person's account password — given it for `sudo`, or that saw the person
  type it into the agent's own terminal — or that gets one approval of a Windows Hello dialog the
  person did not mean to approve, can set a lock password only it knows, make a recovery key only
  it has seen, remove the person's ways in, or turn the lock off. The person is then locked out of
  their own store, or finds its secrets in clear files, and AppRafter offers no reset. Not
  prevented; accepted by the owner. Mitigation: every vault write asks a fresh secret, so an agent
  without one changes nothing even inside the person's unlock window; every surface states the
  cost; the documentation advises agents under a separate OS user or in a sandbox, and
  `--lock-password` where agents get the account password; Re-evaluation.
- **The account password can be guessed offline** by anyone who has a copy of the vault and this
  computer's machine id — every program running as the person, and a full-disk backup of a Linux
  machine. A guessed account password also opens the login, the screen lock, an administrator's
  `sudo`, and on macOS FileVault and the login keychain. Mitigation: the machine binding (answer 1)
  closes copies of the home folder alone; the cost is said before the password is typed, on every
  slot list, in `doctor` and in the guide; one more slot per computer means one more verifier, each
  usable only with that computer's id.
- **A stale account-password slot.** After a password change the previous password keeps opening
  the store, and managing it, until the slot is added again; people change a password because it
  leaked. Mitigation: the hint after a failed attempt, `doctor`'s warning where the change time is
  readable, and the advice to run `lock rekey` after a change made because of a leak.
- **The recovery key becomes the everyday way in** on another computer, after a reinstall, after a
  machine-id change, and — in a container or WSL — possibly after the environment is re-created.
  Mitigation: said in the default plan, the add dialog, the slot list and the guide; `doctor` warns
  and the prompt says so first when only the recovery key opens the store here; a lock password is
  recommended for a store used on more than one computer.
- **A failed check may lock the OS account.** On systems with `pam_faillock`, a failed check from a
  program running as the person counts toward the lockout once the account has a tally file, which
  its first login creates (Appendix). Mitigation: one check per run, a back-off across runs that
  refuses before anything is asked, never a check when a slot opens, the prompt says a wrong
  password may count, and the agent rule of answer 6.
- **A stack that succeeds without the password** (a fingerprint reader, `pam_permit`) would enrol an
  unchecked string; a stack where the password module is optional enrols a typo. Mitigation: the
  "asked once, answered with the typed text" rule, the line before the check ("type the password —
  do not use a fingerprint reader"), and the guide's statement of the optional-module limit.
- **An interrupted key change** refuses every read until it is finished or the lock is turned off.
  Mitigation: the `rotating` state, finished or backed out by the same commands, `lock off` from it,
  a crash test at every step, and `doctor` naming the state.
- **Older AppRafter versions.** A release of the lock's first stage cannot use the new slots, opens
  a default store only with the recovery key, refuses a `rotating` store, and its `lock off` leaves
  this computer's Windows Hello keys behind. Mitigation: stated, not patched; the guide says to
  update every AppRafter that uses the store.
- **The Windows Hello dialog does not name the program that asks.** Mitigation: AppRafter's own
  prompts print the command and its program first; the slot note and the guide say never to approve
  a dialog one did not start.

## Owner

Project maintainers (AppRafter core), on the owner's decisions of 2026-10-15. Later amendments go
through a follow-up record.

## Re-evaluation

- **Revisit after first real use**: once the feature is used, adjust what turns out not to be the
  most convenient or correct.
- If agents that hold the person's account password turn out to be common among AppRafter's users:
  revisit answer 8 (management only with the lock password or the recovery key) and answer 3's
  default.
- If the Windows Hello probe leaves no route (§4).
- When the CLI is signed with a Developer ID (the Touch ID slot).

## References

- [ADR 0068](0068-target-store-lock.md), [ADR 0067](0067-desktop-app-and-apprafter-core.md),
  [ADR 0036](0036-mcp-agentic-safety.md).
- Linux-PAM application developers' guide:
  <https://www.chiark.greenend.org.uk/doc/libpam-doc/html/Linux-PAM_ADG.html>;
  `pam_faillock(8)`: <https://man7.org/linux/man-pages/man8/pam_faillock.8.html>;
  `machine-id(5)`: <https://man7.org/linux/man-pages/man5/machine-id.5.html>.
- OpenPAM on macOS, `pam_opendirectory`; `gethostuuid(2)`.
- Windows Hello: `KeyCredentialManager`
  (<https://learn.microsoft.com/en-us/uwp/api/windows.security.credentials.keycredentialmanager>),
  `KeyCredential.RequestSignAsync`, and the Microsoft Passport key storage provider.
- `MoveFileExW` and `MOVEFILE_WRITE_THROUGH`:
  <https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-movefileexw>.

## Appendix: what the platform checks found (2026-10-15)

Each check ran against accounts it created: `walker` in throwaway Debian 13 and Fedora 42
containers, and accounts made on a GitHub-hosted macOS 26 runner (arm64) and inside WSL
distributions on a Windows Server 2025 runner. None ran on a person's machine or account.

**Linux PAM, from a program running as a normal user.**

- `libpam.so.0` loads at run time on both. With the shipped stacks (`common-auth` on Debian,
  `system-auth` on Fedora, and `login`) one `pam_authenticate` asks exactly one hidden prompt and
  succeeds with the right password and fails with a wrong or empty one; a password set beforehand
  as `PAM_AUTHTOK` is ignored there.
- A `pam_permit` stack, and a fingerprint-like stack (`pam_permit` before the password module),
  succeed without asking anything. A stack where the password module is `optional` before a
  `sufficient` module succeeds after a wrong password. Counting the prompts tells "checked" from
  "not checked" in the first two cases only.
- With `pam_faillock` (deny 3): when the account's tally file does not exist, a normal user's
  failures are not recorded (it cannot create the file). The file is created, owned by the account,
  the first time a check runs as root for it — a login, `su`, `sudo` — and from then on the user's
  own failures are recorded: three of them locked the account, after which the right password
  failed both for the program and for `su`, until the unlock time or a reset. On Debian the wrong
  password then returns "permission denied" rather than "authentication failure".
- `passwd -S` for one's own account works without privileges on both, and says whether the password
  is set, locked or empty. `chage -l` works too. Both give the last change as a date, not a time.

**macOS (PAM through OpenPAM, and Open Directory), from a program running as a normal user.**

- The PAM service `chkpasswd` (`pam_opendirectory` alone) asks exactly one hidden prompt and
  succeeds only with the typed password: the same rule as on Linux. `checkpw` asks nothing and
  succeeds when the password was set beforehand as `PAM_AUTHTOK`. The `passwd` service succeeds
  without any password (`pam_permit`) and must never be used. `login`, `sshd`, `su` and `sudo` also
  verify, but carry other modules (Kerberos, NTLM, a local `sudo` include where Touch ID may sit).
- `libpam.2.dylib` loads at run time from the system's shared cache.
- Open Directory's record verification checks the password too, for the caller's own account and
  for any other local account, and accepts an empty password on an account whose password is
  empty.
- Failed checks, through PAM or Open Directory, raise the account's failed-login count, which a
  password policy may use to lock it; a successful check resets it.
- A user reads their own password-change time (`passwordLastSetTime`, to the sub-second) without
  privileges; after a change the old password failed at once. An empty password is not readable as
  a fact (the account's authentication record looks like any other); an account with no password
  record at all is; a disabled account shows as "permission denied" at the check itself.
- `gethostuuid()` returns the hardware UUID's 16 bytes without privileges.
- The per-user temporary directory does not move with `HOME` or `TMPDIR`.

**Machine ids and runtime directories.**

- Container images: Debian 13's slim image has no `/etc/machine-id`, Fedora 42's an empty one, and
  the container runtime adds none — such a container has no machine id. An id written inside a
  running container survives a restart and is gone when the container is re-created. An id written
  while an image is built (installing systemd does) is the same in every container made from that
  image, across restarts and re-creates.
- WSL: with WSL 1 the distribution's `/etc/machine-id` is empty. With WSL 2 and systemd (Debian 13's
  default there) it is generated on first start, stays the same across a WSL restart, and is new
  after the distribution is removed and installed again.
- `/run/user/<uid>` does not exist in containers or under WSL 1; under WSL 2 with systemd a normal
  user gets one.
- Toolbox and Distrobox were not checked.
