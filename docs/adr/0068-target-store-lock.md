# ADR 0068: Target-store lock — a store's secrets in an encrypted vault, opened per terminal

## Status

`Accepted` (2026-10-12).

Date: 2026-10-12; amended in place 2026-10-13 before implementation, with the answers to the
questions the D.4a plan raised, and §8's wording, with the phrase of ADR 0067's Status that
quotes it, on 2026-10-14 (where the desktop shows the store's protection; the decision is
unchanged). ADR for `plan.md` Phase D (sub-phase D.4). Amends
[ADR 0067](0067-desktop-app-and-apprafter-core.md): it replaces the keyring bullet of its §5 and
qualifies the other parts that ADR 0067's Status lists; §10 below names each.

## Context

A target store (the directory `APPRAFTER_CONFIG_DIR` names, `~/.config/apprafter` by default)
holds three kinds of secret. Any program running as the same OS user can read all of them:

- each target's Hetzner API token, in plain YAML in `targets/<name>/credentials.yaml`, mode 0600
  ([ADR 0030](0030-cli-target-store-and-credential-chain.md) put its encryption out of scope);
- the cached kubeconfig and Argo CD admin password, age-encrypted in each target's state file
  (and, in older state files, a plaintext kubeconfig);
- the age identity that decrypts them: one file per machine, `~/.config/apprafter/age.key`,
  shared by every store on that machine.

ADR 0067 §5 planned to move the age identity and the tokens into the OS keyring (Keychain,
Credential Manager, Secret Service) by default, with an explicit migration command for existing
stores. Two findings changed that plan before it was built.

**The threat.** AI coding agents with shell access run as the developer's own OS user.
[ADR 0036](0036-mcp-agentic-safety.md) keeps an agent away from destructive operations by
structure: the MCP surface cannot express them, and a `MigrationPlan` needs an out-of-band human
approval, which ADR 0036 places in the `apprafter` CLI and the Argo CD approve and reject buttons.
On the machine where that CLI is configured, an agent with a shell does not need the MCP surface.
It can run `apprafter migration approve` itself, or read the token and the age identity, decrypt
the kubeconfig and the Argo CD admin password, and act directly. An OS keyring does not close
this. The Secret Service protocol, and gnome-keyring, have no per-application access control, so
any client on the user's session bus reads an unlocked item. Any process of the user reads a
Windows generic credential. A macOS Keychain item trusts the binary that created it, which is the
same `apprafter` binary the agent can run. The desktop's application lock (ADR 0067 §5) receives
a yes or a no from the OS, not a key: it gates the app, not the secrets behind it.

**The cost of moving secrets.** A migration that deletes the files leaves the OS store as the
only copy, and a review found ways to lose or misread it: a locked or unreachable keyring taken
for an absent entry, a change of keyring provider, a store copied together with the identifier
its keyring entries were named by. The CLI must never lose access to targets because of a
migration.

## Decision

### 1. An opt-in lock, not a new default

We will add an opt-in **target-store lock**. Secrets stay in files by default. `apprafter lock
on` moves one store's secrets into an encrypted vault. The vault opens only with a slot's secret
or an OS gesture that yields key material. There is no OS keyring backend and no migration
command.

What the lock guarantees, against the two attacker models of ADR 0036:

- **An agent that oversteps (excessive agency).** The lock stops it at the store. An agent that
  runs the CLI gets a refusal or a prompt it cannot answer; an agent that reads the store's files
  gets ciphertext. Nothing the store holds is on disk in clear. The routes that stay open are
  named below.
- **A deliberately malicious process of the same user** (an agent turned by prompt injection, for
  example). The lock raises the bar but does not close it. Processes of one OS user share one
  trust domain: such a process can capture a secret as it is typed, change what the person runs,
  or act inside an unlocked session (Risks lists each route). For this case the defence is an OS
  boundary: run the agent under a separate OS user, or in a sandbox that cannot reach the
  person's terminals, files and sessions. The documentation says so, and says how.

Two routes stay open even to an agent that only oversteps. `lock on`, `lock status` and the
documentation say that the lock protects secrets at rest, and name both:

- **The SSH key that fetches kubeconfigs.** It stays outside the vault. A process the lock refuses
  can still use that key, or an SSH agent that holds it, to read the cluster's kubeconfig from the
  node. `doctor` warns when that key has no passphrase or is loaded in an SSH agent
  (`ssh_key_exposure`). The documentation recommends a dedicated, passphrase-protected key per
  target, never loaded into an SSH agent that an AI agent can reach, and AI agents under a
  separate OS user.
- **Secrets in use.** While a command or the unlocked desktop uses a secret, it is in that
  process's memory. A kubeconfig handed to `kubectl` is also a short-lived file that programs
  running as the same user can read, and a command that puts the Argo CD password on the
  clipboard or forwards a local port leaves both open to them. `doctor` states this as
  information (`secrets_in_use`).

An SSH key per target kept in the vault, and a kubeconfig that is never a named file, are later
work.

### 2. Data model

The lock is per target store.

- **`<root>/store.yaml`**, a new file, holds its own `format` version, the store's `id` (its use
  is settled in the D.4a plan), `secrets: files | locking | locked | unlocking`, the slot list
  (each slot's type and label, never a secret), and a copy of the default unlock timeout for
  display. `locking` and `unlocking` mark an interrupted `lock on` and `lock off` (§5).
  `config.yaml` cannot carry this: the CLI rewrites it without fields it does not know, and
  `target remove` deletes it when no target is left.
- **`<root>/vault`**, mode 0600, holds a random data key, wrapped once per slot. Under the data
  key (XChaCha20-Poly1305) it holds every Hetzner token of the store's targets, the store's
  **own age identity**, created at `lock on`, and the default unlock timeout. The vault's
  timeout is the one that counts: a process that can edit `store.yaml` must not be able to
  lengthen every future window. The caches the shared age identity encrypted (the kubeconfig and
  the Argo CD password) are re-encrypted to the store's own identity; where those caches live is
  open (see Risks).
- The vault starts with a versioned header. A released format version never changes meaning;
  golden vectors (a fixed salt, key and ciphertext) freeze each one.
- The vault is never decrypted to a file. The token fields of `credentials.yaml` and the caches
  under the shared age identity are removed only after the vault has been written and decrypted
  again. The shared `age.key` and every other store stay as they are: locking one store never
  takes access away from another.

### 3. Slots

A slot is one way to open the vault, as in LUKS. The user chooses slots at `lock on`:

- **Password**: a lock password; argon2id derives its wrapping key, with the parameters stored in
  the slot. Release builds refuse weak parameters; fast ones exist only behind a test feature.
  AppRafter sets no minimum length. A lock password must not be empty, and must not have the
  shape of a recovery key, because one prompt takes either secret. `lock on` and the desktop
  warn when it is shorter than 15 characters and say why: anyone who copies the vault file, an
  agent running as the user included, can guess the password offline, and argon2id only slows
  each guess.
- **Recovery key**: always present; device-independent; shown once, and confirmed by typing the
  whole key back before anything is written, so a mistake in the person's copy shows at once
  rather than when the key is the last way in. The desktop copies it to the clipboard only from
  its own process, never from its web view, marked to stay out of clipboard history and cloud
  clipboard sync, and clears it after 60 seconds.
- **OS password** (Linux, macOS): the account password, typed into AppRafter's own prompt.
- **Windows Hello**: the Hello prompt, used as a source of key material.
- **Touch ID**: staged until the CLI is signed with a Developer ID; the mechanism is open.

Every store keeps at least one device-independent slot: a password or the recovery key. When a
slot keeps an item in Windows Credential Manager, the item is persisted as `Enterprise`
(`CRED_PERSIST_ENTERPRISE`). With a roaming profile it then follows the user to other computers,
as the target store in the roaming application-data folder does; without one it stays local. A
Windows Hello key stays on the device where it was enrolled, so a roamed store opens on another
computer only through a device-independent slot.

### 4. The per-terminal holder

`apprafter unlock` reads a slot's secret from the terminal without echo (`/dev/tty`; the console
on Windows), and so does the first command on a locked store that needs a secret while a terminal
is present. The prompt names the store, the command that asks, and the process that started it
(its name and pid), so a person can tell their own command from one an agent runs in their
terminal.

A successful unlock starts a background **holder** for that store and that terminal session. It
keeps the data key in its own memory only. On Linux it sets `PR_SET_DUMPABLE` to 0. On macOS it
calls `ptrace(PT_DENY_ATTACH)`, which refuses a ptrace attach; memory reads through
`task_for_pid` are governed by macOS's task-port policy instead, which requires the holder to be
built without the `get-task-allow` entitlement and which a machine set up for debugging can relax.
It locks the key's pages, best effort (`mlock`; `VirtualLock` on Windows), and zeroes the key on
exit. It listens on a socket in `$XDG_RUNTIME_DIR/apprafter/`. When that variable is unset, Linux
uses the store's own `<root>/run/` and macOS `$TMPDIR/apprafter/`, then the per-user temporary
directory; `lock status` names the fallback. The directory is 0700, and the socket 0600 with the
sticky bit set, which keeps a cleaner of `$XDG_RUNTIME_DIR` away from it. On Windows the holder
listens on a named pipe restricted to the user's SID. These permissions keep out other users only
(see Risks). The holder serves a client only when:

- **Linux and macOS:** the client runs under the same uid **and** in the same session as the
  shell where the secret was typed. The peer's uid and pid come from the socket: `SO_PEERCRED` on
  Linux; on macOS, `LOCAL_PEERPID` and `getpeereid`.
- **Windows:** the client is attached to the same console (in Windows Terminal, the same pane),
  checked with `GetConsoleProcessList`. That call lists only the caller's own console, so the
  holder has to be attached to that console to check it; how the holder then ends when the shell
  exits is open. This binds the unlock window to a console, not to a person: any non-elevated
  process of the same user can attach itself to that console (`AttachConsole`) and then passes
  the check, and the client pid a named pipe reports (`GetNamedPipeClientProcessId`) can be
  spoofed. On Windows the unlock window is open to every process of the same user while it lasts.

The **unlock window** lasts N minutes, 5 by default and at most 1440 (a day), and each use
refreshes it; `0` means every command asks. It ends on timeout, on `apprafter lock now`, when the
terminal closes or the shell exits, and when the computer sleeps, as the desktop app locks then
(§8). The holder sees sleep by comparing a clock that counts suspended time with one that does
not. A hibernation image still holds the key of a window that was open; `lock status` and the
documentation say so.

With no terminal and no holder, a command refuses at once with a `store_locked` error ("this
target store is locked: a person must run `apprafter unlock` in their terminal"); its full error
code is settled in the D.4a plan. It never waits for input.

### 5. Commands

| Command | Does | Confirmation |
|---|---|---|
| `lock on [--timeout <min>]` | creates the vault, its slots and the store's age identity; moves the tokens; re-encrypts the caches; the plan lists what moves where. On a locked store, `--timeout` changes the default unlock window | a bounded plan, the password twice (with a warning when it is shorter than 15 characters), the recovery key typed back in full; on a locked store, `--timeout` asks for a slot's secret, even inside an unlock window |
| `lock off` | puts everything back into files (tokens into `credentials.yaml`, caches under the shared age identity: `age.key`, or the file `APPRAFTER_AGE_KEY` names (§6), created as on a first use if absent) and deletes the vault | a slot's secret, asked even inside an unlock window, and a plan that warns that the secrets will be on disk in clear again |
| `unlock [--timeout <min>]` | ends this terminal's holder if one runs and starts a new one | a slot's secret, asked even inside an unlock window |
| `lock now` | ends the unlock window in this terminal | none |
| `lock status` | locked or not, the slots, the unlock window left in this terminal, this machine's limits | none |
| `lock slot add`, `remove`, `list`; `lock rekey`; `lock recovery` (D.4b) | manage OS-password and Windows Hello slots (Touch ID slots once they exist); change the password; issue a new recovery key | a slot |

- Every command that needs a secret goes through a single secret-store interface
  (`SecretStore`): `apply`, `destroy`, `kubeconfig`, `argocd-password`, `migration approve`, the
  application commands and the rest. Inside an unlock window a read answers without a prompt, for
  every command. Without one, the CLI asks inline at a terminal; with no terminal it refuses with
  `store_locked`. The core itself never prompts (ADR 0067 §2).
- Four things ask for a slot's secret even inside an unlock window, whose holder serves reads
  only: a CLI command that writes the vault (`target add`, `target add --renew`, `target rename`,
  `target remove` and `import`, on a locked store), `unlock`, `lock on --timeout` on a locked
  store, and `lock off`. An agent started inside a person's window can use the store's secrets
  while the window lasts, but cannot add a target, replace a stored token, lengthen future
  windows or remove the lock.
- No other command asks again inside a window, `migration approve` included: any process the
  window serves can read the kubeconfig and act with `kubectl` directly, so a second prompt there
  would protect nothing. A stronger gate for approvals is later work.
- `lock on` runs in two phases and can be resumed: it sets `secrets: locking`, writes the vault,
  verifies it, deletes the clear data, and sets `secrets: locked`. Running `lock on` again
  finishes an interrupted run, and `doctor` names that state. `lock off` runs the same way
  through `secrets: unlocking`, and on a `locking` store it backs out of the interrupted
  `lock on`, so a person can always return to files.

### 6. Overrides on a locked store

`HCLOUD_TOKEN` and `APPRAFTER_AGE_KEY` do not apply to a locked store: the CLI refuses with a
typed error that says why. Otherwise an agent holding a token obtained elsewhere would pass the
lock silently. One use is not an override: `lock off` writes the caches back under a shared age
identity, and with `APPRAFTER_AGE_KEY` set it takes that file as the identity's destination
(loaded, or created as on a first use) and its plan names the path. That lets nothing past the
lock, since `lock off` asks for a slot's secret anyway. On a store in files, both keep working as
ADR 0067 §2 describes.

### 7. `doctor` and `status`

- Locked: `Secrets: locked (2 slots: password, recovery) · unlocked in this terminal, 3 min left`.
- In files: `Secrets: in files, readable by any program running as you — apprafter lock on
  protects them`. Informational, not a warning: files are the deliberate default; the row states
  the fact.
- The machine's limits (keystrokes under X11 or Xwayland; Windows) and an interrupted `lock on`
  have rows of their own. Which of the further limits under Risks get a row is open.
- Two rows state what the lock does not cover (§1): `ssh_key_exposure`, a warning on a locked
  store when the SSH key that fetches kubeconfigs has no passphrase or is loaded in an SSH agent,
  and `secrets_in_use`, information that a secret in use is in a process's memory and a
  kubeconfig in a file that programs running as the user can read.

### 8. The desktop: one lock, in Settings › Lock and on the Security page

*Wording amended in place on 2026-10-14: where the desktop shows the store's protection. The
decision is unchanged.*

The application lock and the store lock become one mechanism. Settings › **Lock** holds how the
app locks; the target store's protection has a page of its own, **Security**, in the sidebar,
which names the store and every target in it, because the lock covers the whole store:

1. Settings › Lock: *Require unlock* and *Lock when the computer sleeps or locks*, as today.
2. Security, *Protect secrets*. Off: "Secrets are in files; the lock only covers this app's
   window", with **Protect** (`lock on`). On: "Secrets are encrypted; unlocking opens them".
3. Security, the slots, shown while protection is on: list, add and remove, recovery key,
   password change.

While protection is on, *Require unlock*, *Lock when the app starts* and *Lock when the computer
sleeps or locks* cannot be turned off, and *Auto-lock after inactivity* offers no *Never*: the
app's unlock is what opens the vault, and the app's unlocked time should end as a terminal's
window does. The person's own choices are kept and return when protection is turned off. On a
locked store that unlock goes through a slot: Windows Hello, now yielding a key; Touch ID once
signed; on Linux, the app's own password field, feeding the OS-password or lock-password slot.
polkit's dialog yields no key, so on Linux the password field replaces it for a locked store, and
the desktop says so, in Settings › Lock and on the Security page. The desktop does not use the
terminal holder: it keeps the data key in its own process while the app is unlocked, and the app
lock or sleep drops it.

Until a gesture slot exists, only the password and recovery-key slots open a locked store. How the
desktop opens one on Windows and macOS in D.4a, where the system prompt the app lock uses yields
no key, and whether the PAM check stays on Linux, is open.

### 9. Staging and the test safety net

- **D.4a:** the vault; `lock on`, `lock off`, `unlock`, `lock now`, `lock status`; the holder; the
  password and recovery-key slots; the `doctor` and `status` rows; the desktop's Security page
  and Settings › Lock.
- **D.4b:** the OS-password slot (Linux, macOS), the Windows Hello slot, and slot management
  (`lock slot add`, `remove` and `list`; `lock rekey`; `lock recovery`).
- **Later:** the Touch ID slot, staged until the CLI is signed with a Developer ID.

The first D.4 change is a test safety net: tests spawn the CLI from one place, with a clean
environment and scratch `APPRAFTER_CONFIG_DIR`, `XDG_RUNTIME_DIR` and `HOME`, and a guard test
fails if any test can reach the user's own stores, a holder, or (from D.4b) the OS's Hello or
Keychain.

### 10. Relation to other decisions

- **ADR 0036.** Its approval gates now also hold on the machine where the CLI is configured,
  against its first attacker model (excessive agency): with the store locked, an agent with a
  shell can neither decrypt the store's files nor use the CLI's stored credentials, unless it
  acts inside an unlock window, and even there it cannot write the vault or remove the lock
  without a slot's secret (§5). Against its second model (a deliberately malicious agent) they
  hold only with an OS boundary between the agent and the person (Decision §1).
- **ADR 0067.** This replaces the fourth bullet of §5 (the OS keyring) and qualifies its first
  bullet for locked stores: the desktop's unlock opens the vault through a slot, and on Linux the
  app's own password field replaces polkit there. It also changes the Risks entry "macOS Keychain
  and unsigned binaries": signing no longer decides where secrets are stored, and the Touch ID
  slot is staged after it. The rejected alternative "Keep secrets as they are and lock the UI
  only" becomes the state of a store in files, with the desktop's Security page saying that the
  lock then covers only the app's window. The rest of §5 stands.
- **ADR 0030.** ADR 0067 superseded its "encryption out of scope" note for the age identity and
  the provider tokens. This record narrows that: the note is superseded for locked stores, whose
  secrets are encrypted at rest, and stands for stores in files.

## Consequences

- The secrets in a locked store's files are useless to a same-user agent that reads them, and
  the CLI does not open them without a slot's secret entered at a terminal (a pseudo-terminal an
  agent allocates counts as one, so the guarantee rests on the agent not knowing the secret).
- Protection against a deliberately malicious agent needs an OS boundary the lock does not
  provide: the documentation, and `lock on` itself, recommend running agents under a separate OS
  user or in a sandbox.
- Because the lock is per store, an agent can be given a store of its own: lock the stores that
  hold real targets and keep a separate, unlocked store (its own `APPRAFTER_CONFIG_DIR`) for a test
  or experimental target the agent may drive freely. The guide describes this layout.
- No OS keyring holds the store's secrets. A gesture slot (D.4b or later) may keep an item in an
  OS store as one more way to unwrap the same data key; on Windows such an item is persisted as
  `Enterprise`. So there are no D-Bus timeouts, no keyring-provider changes, no per-OS entry
  semantics, and no signing prerequisite for storing secrets. One vault format serves all three
  OSes.
- Nothing moves by default: an upgrade changes no store, and scratch stores are untouched. Both
  layouts, files and vault, are supported.
- A person unlocks each terminal, again after the unlock window expires or the computer sleeps,
  and once more for each vault write, `lock off` or change of the default window; that friction
  is the point. The holder is a new background process whose peer checks differ, and are tested,
  per OS.
- A short lock password is the person's choice, made after a warning; the documentation says
  what it costs.
- On a locked store, scripts that rely on `HCLOUD_TOKEN` or `APPRAFTER_AGE_KEY` stop working.
- While the desktop is locked, nothing in it can decrypt a locked store's secrets, background work
  that needs a kubeconfig included, because the app lock drops the key.

## Alternatives considered

- **The OS keyring as a backend** (ADR 0067 §5). Rejected: a same-user agent reads Secret Service
  items and Windows generic credentials, or runs the binary the macOS Keychain trusts; and the
  keyring brings the migration hazards described in Context.
- **A refusal in the CLI alone**: sensitive commands wait for a person at a terminal while secrets
  stay in files. Not chosen: the threat model includes an agent that reads the files directly,
  which such a refusal does not stop.
- **A privileged Linux helper, authorised through polkit**, holding the key out of every
  same-user process's reach, and **FIDO2 `hmac-secret`**, a hardware security key whose
  per-credential secret could wrap the data key. Considered; no decision is recorded on either.

## Risks

Where a risk says "Mitigation: open", the D.4a plan (or the D.4b plan, where named) settles it.

- **An unlocked terminal is an unlock window for everything in it.** Every process started from
  it inherits the unlock window, an agent started there included. Mitigation: `doctor`, `lock
  status`, `unlock`'s output and the documentation say so; unlock in a separate terminal, or use
  timeout `0`. Inside a window, vault writes, `unlock`, `lock on --timeout` and `lock off` still
  ask for a slot's secret (§5).
- **Capturing the typed secret.** A same-user process can open the person's terminal device
  (`/dev/pts/N` on Linux, `/dev/ttysNNN` on macOS) for reading and race the prompt for the typed
  line; on Windows it can attach to the console and read its input. It can also change what the
  person runs: shell startup files, `PATH`, or a binary in a user-writable prefix. Once it holds
  the secret it needs no person: any process can allocate a pseudo-terminal and answer the
  prompt. Mitigation: this is the deliberately malicious case of Decision §1, answered by an OS
  boundary (a separate OS user or a sandbox), which the documentation describes; the D.4b gesture
  slots remove the typed-secret route for the people who use them. Whether the D.4a plan adds
  cheaper checks (for example, refusing to prompt when the terminal device is readable by another
  open file) is open.
- **Entering the unlocked session from outside.** A same-user process can act inside the session
  the holder serves without a debugger, through a terminal multiplexer or emulator that accepts
  commands on a same-user socket (tmux `send-keys`, screen `-X stuff`, `wezterm cli send-text`).
  On Linux with Yama `ptrace_scope` 0 (Fedora's default) or without Yama, it can also attach a
  debugger to the shell. Mitigation: as for the previous risk, an OS boundary for the deliberately
  malicious case; `doctor` reports `ptrace_scope` 0, and the documentation names the multiplexers.
- **The Windows console binding.** Any process of the same user can attach itself to the unlocked
  console and pass the check, and the client pid of a named pipe can be spoofed (§4). Microsoft
  also marks `GetConsoleProcessList` as not recommended and warns that it may not work as expected
  over SSH. The planned `windows-latest` test, that a process on another console is refused, does
  not cover a process that attaches itself to the unlocked console. Mitigation: open — what the
  binding and its tests become, a manual test on a real Windows machine, and whether a client
  connected over SSH is served or refused.
- **Same-user memory reads.** On Windows a process of the same user can read the holder's memory
  unless the holder restricts its own process and thread DACLs, which needs no administrator
  rights; an elevated process can still read it. On macOS `PT_DENY_ATTACH` refuses only ptrace,
  and reads through `task_for_pid` depend on the task-port policy (§4). Mitigation: `doctor`
  reports the Windows limit and the documentation states it. Open: whether the holder restricts
  its DACL on Windows; whether it is built with the hardened runtime on macOS (an ad-hoc
  signature, `codesign -s - -o runtime`, needs no Developer ID); whether `doctor` reports a macOS
  limit.
- **The desktop process holds the data key.** It is an ordinary GUI process: under Yama
  `ptrace_scope` 0, or without Yama, a same-user process can attach to it, and on Windows it can
  read its memory unless the process restricts its own DACL. Mitigation: open — which of the
  holder's protections (`PR_SET_DUMPABLE` 0, a restricted process DACL, the macOS task-port
  policy) the desktop process applies.
- **Keystrokes under X11 or Xwayland.** Under X11, or for a terminal running under Xwayland, any
  X client of the same user sees keystrokes, the unlock secret included (see also "Capturing the
  typed secret", which needs no display server). We accept this; `doctor` reports it and the
  documentation states it. Open: how `doctor` tells that a terminal runs under Xwayland, since the
  session type alone does not say.
- **The holder's endpoint.** The directory, socket and pipe permissions keep out other users
  only. A same-user process can remove the socket and bind its own at the same path, or create
  the pipe name before the holder does, so a client may reach a fake holder. Mitigation: open —
  whether anything secret crosses the endpoint (the refresh path of `unlock`), and how a client
  authenticates the holder (on Windows, `FILE_FLAG_FIRST_PIPE_INSTANCE` and
  `PIPE_REJECT_REMOTE_CLIENTS` apply).
- **Secrets outside the vault.** The kubeconfig that `kubectl` reads is written to a short-lived
  owner-only file during each command that runs it and during each desktop poll. A kubeconfig the
  user saved from `apprafter kubeconfig` stays where it was saved. The SSH private key the target
  uses stays outside the vault and fetches a new kubeconfig from the node. An older state file
  may still hold a plaintext kubeconfig. Mitigation: stated, not closed (Decision §1): `lock on`,
  `lock status` and the documentation name the SSH key and the secrets in use, `doctor` reports
  them (`ssh_key_exposure`, `secrets_in_use`, §7), and the documentation gives the advice of §1;
  a vault-held SSH key and a kubeconfig that is never a named file are later work. Open: what
  `lock on` does with a plaintext kubeconfig left in a state file.
- **Lost-key recovery on a locked store.** `apprafter kubeconfig --refresh` and `doctor` look for
  the age identity as a file. On a locked store the identity is in the vault, and the shared
  `age.key` that stays on disk cannot open that store's caches. Mitigation: open — which identity
  a locked store's reads use, whether a refresh may create a new store identity, and what `lock
  on` does with caches it cannot decrypt.
- **Every slot lost, or the vault file lost.** The vault cannot be opened, and the recovery key
  opens nothing without the vault file. AppRafter offers no reset. Mitigation: the recovery key
  always exists and is typed back in full at `lock on`; `lock on` and the documentation say to
  keep it somewhere other than this computer, and to include `<root>/vault`, which is encrypted,
  in backups. The documentation gives the manual way back: move the vault and `store.yaml` aside,
  enter each target's token again, and fetch each kubeconfig again from its node, which needs SSH
  access to the node.
- **A weak lock password.** A copied vault file can be attacked offline, where argon2id only
  slows each guess, and AppRafter sets no minimum length. Mitigation: `lock on` and the desktop
  warn below 15 characters and say why (§3); release builds refuse weak argon2id parameters.
- **Failures in the vault code.** A decryption failure read as an absent secret would let a new
  key overwrite the only copy; an interrupted `lock on` could lose clear data; a test could reach
  a real store or holder. Mitigation: a failed authenticated decryption is a typed refusal, never
  "no such secret"; tamper tests and golden vectors; no clear data deleted before a verified
  vault, `lock on` interrupted at every step in tests; the safety net first; guards
  mutation-tested.
- **Gesture slots that yield keys.** The Hello call the app lock uses today
  (`UserConsentVerifier`) returns a verdict, not key material. A known route derives a key from a
  Hello signature over a fixed challenge (`KeyCredentialManager.RequestSignAsync`), which assumes
  the signature is deterministic. A Touch ID-protected Keychain item needs the data-protection
  keychain, whose entitlements must be authorised by a provisioning profile that a bare
  command-line binary cannot carry outside an app-like bundle; a Secure Enclave key with
  biometric access control is another route. An item kept in Credential Manager is readable by
  any process of the user, so it cannot open the vault by itself. Mitigation: these slots come in
  D.4b or later, and no other slot depends on them. Open: the mechanism of each.
- **An OS-password slot after a password change.** It will not open with the new account password.
  Mitigation: the device-independent slot. Open (D.4b plan): re-enrolling the slot.
- **An older CLI on a locked store.** It does not read `store.yaml`, finds no tokens and caches it
  cannot decrypt, and writes in the old layout. Mitigation: open.

Also open for the D.4a and D.4b plans:

- where the caches re-encrypted to the store's own identity live, and how a new cache is written
  on a locked store without an unlock;
- a socket name that fits macOS's 104-byte `sun_path`;
- a store copied together with its `id`, which a holder could serve;
- the recovery key's derivation;
- the codes of `store_locked` and of the override refusal (§6);
- how `doctor` shows the informational "in files" row, since it has no informational status
  today;
- the plan class of `lock off`, `lock slot remove` and `lock rekey`.

## Owner

Project maintainers (AppRafter core). This record was amended in place on 2026-10-13, before any
of it was implemented, and §8's wording on 2026-10-14 (where the desktop shows the store's
protection; the decision is unchanged); later amendments go through a follow-up ADR.

## Re-evaluation

- When the CLI is signed with a Developer ID (the Touch ID slot).
- When the D.4a and D.4b plans settle the risks this record leaves open.

## References

- [ADR 0030](0030-cli-target-store-and-credential-chain.md),
  [ADR 0036](0036-mcp-agentic-safety.md), [ADR 0067](0067-desktop-app-and-apprafter-core.md).
- RFC 9106, Argon2: <https://www.rfc-editor.org/rfc/rfc9106.html>.
- NIST SP 800-63B-4 (15 characters as the minimum for a password used as a single factor):
  <https://pages.nist.gov/800-63-4/sp800-63b.html>.
- XChaCha20-Poly1305:
  <https://doc.libsodium.org/secret-key_cryptography/aead/chacha20-poly1305/xchacha20-poly1305_construction>.
- Secret Service API: <https://specifications.freedesktop.org/secret-service-spec/latest/>.
- Linux: <https://man7.org/linux/man-pages/man2/PR_SET_DUMPABLE.2const.html>,
  <https://man7.org/linux/man-pages/man7/unix.7.html> (`SO_PEERCRED`),
  <https://docs.kernel.org/admin-guide/LSM/Yama.html>,
  <https://specifications.freedesktop.org/basedir/latest/> (`$XDG_RUNTIME_DIR`),
  <https://fedoraproject.org/wiki/Changes/Restrict_ptrace_for_unprivileged_users_to_child_processes_to_match_kernel_default>
  (Fedora's `ptrace_scope`), <https://lwn.net/Articles/991438/> (Xwayland and X11 clients).
- Terminals: <https://wezterm.org/cli/cli/send-text.html>.
- macOS: `ptrace(2)`, `PT_DENY_ATTACH`:
  <https://developer.apple.com/library/archive/documentation/System/Conceptual/ManPages_iPhoneOS/man2/ptrace.2.html>;
  a Secure Enclave key with biometric access control in a command-line tool:
  <https://github.com/remko/age-plugin-se>.
- Windows: <https://learn.microsoft.com/en-us/windows/console/getconsoleprocesslist>,
  <https://learn.microsoft.com/en-us/windows/console/attachconsole>,
  <https://learn.microsoft.com/en-us/windows/win32/procthread/process-security-and-access-rights>,
  <https://learn.microsoft.com/en-us/windows/win32/api/wincred/ns-wincred-credentialw>
  (`CRED_PERSIST_ENTERPRISE`),
  <https://learn.microsoft.com/en-us/windows/win32/api/memoryapi/nf-memoryapi-virtuallock>,
  <https://learn.microsoft.com/en-us/windows/win32/dataxchg/clipboard-formats> (clipboard data
  kept out of clipboard history and cloud clipboard),
  <https://projectzero.google/2019/09/windows-exploitation-tricks-spoofing.html> (named-pipe
  client pid), <https://github.com/keepassxreboot/keepassxc/blob/develop/src/core/Bootstrap.cpp>
  (a process DACL restricted without administrator rights).
