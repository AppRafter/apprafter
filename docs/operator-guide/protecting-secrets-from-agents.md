---
description: "Lock a target store's secrets in an encrypted vault so that an AI agent that oversteps cannot read or use them without you — and what the lock does not stop. Unlocking a terminal, giving an agent a store or an account of its own, and backing the vault up."
---

# Protecting secrets from agents

An AI coding agent with a shell runs as you. Whatever `apprafter` can do on
your machine, the agent can do too, and it can read the files the CLI keeps:
each target's Hetzner API token, the cached kubeconfig and Argo CD admin
password, and the key that decrypts them.

`apprafter lock on` moves one target store's secrets into an encrypted vault
that opens only with your lock password or its recovery key, typed at a
terminal. This page is the recipe; how the vault, the unlock window and the
terminal binding work is [How the store lock works](../how-it-works/the-store-lock.md).

!!! warning "There is no reset"
    AppRafter cannot open a locked store without its lock password or its
    recovery key. Keep the recovery key somewhere other than this computer.
    The recovery key opens nothing without the vault file: include the
    store's `vault` in your backups — it is encrypted, and a copy is as
    strong as your lock password. If every way in is lost, the way back is
    to [rebuild the store's secrets from their
    sources](troubleshooting.md#locked-store-lost).

## What the lock covers {#what-it-covers}

The lock stops an agent that oversteps — one that runs `apprafter`, or reads
the store's files, to do something you did not ask for. On a locked store:

- a command that needs a secret asks for the lock password at its own
  terminal. A command started without one — a script, a CI job, an agent's
  tool — is refused with
  [`apprafter::secrets::store_locked`](troubleshooting.md#store-locked);
- at rest, the store's files hold no secret in clear: the tokens and the
  store's own age key are in the vault, and the cached kubeconfigs and Argo
  CD passwords are encrypted to that key. A secret an older AppRafter or
  another program left in clear is never used, and `apprafter doctor`
  reports it;
- `HCLOUD_TOKEN` and `APPRAFTER_AGE_KEY` are refused, so a token obtained
  elsewhere cannot pass the lock unnoticed
  ([`override_refused`](troubleshooting.md#override-refused)).

The exception is a terminal you have unlocked: everything started from it,
an AI agent included, uses the store's secrets until the window ends —
`apprafter migration approve` included.

The lock protects secrets at rest. While a command or the app uses one, it is
in that process's memory and, for a kubeconfig, in a file programs running as
you can read; the SSH key that fetches kubeconfigs stays outside the lock.

Against a program running as you that sets out to get past the lock — an
agent turned by a prompt injection, for example — the lock only raises the
bar: it slows such a program, and does not stop it. Such a program can read
the lock password as you type it, change what your shell runs, or type into a
terminal you have unlocked. Against that, the defence is a boundary the
operating system enforces: [run the agent under another account without
administrator rights, or in a sandbox](#a-boundary-for-the-agent). Neither
the lock nor a separate account protects anything from an administrator of
this computer.

## Lock a target store {#lock-on}

In a terminal of your own — not one an agent runs in:

```sh
apprafter lock on
```

It shows what moves where, asks for a new lock password twice, shows a
recovery key once and asks you to type it back in full, and then moves the
secrets. The store is locked when it ends, and no terminal is unlocked yet.

**The lock password.** Use 15 characters or more: anyone who copies the vault
file — an AI agent running as you can — can try passwords against it
offline, as fast as their hardware allows; the key derivation only slows
each guess. AppRafter sets no minimum, and warns below 15. A phrase of
several words is easier to type and harder to guess. A password cannot be
empty, and cannot have the shape of a recovery key, because one prompt takes
either.

**The recovery key.** It opens the store if the lock password is lost, and it
is shown only once. Copy it somewhere other than this computer — a password
manager on another device, or paper — then type it back when asked: a mistake
in your copy shows now rather than on the day you need it. Clear the screen
afterwards; a terminal multiplexer keeps scrollback of its own.

**How long an unlock lasts.** 5 minutes by default. `--timeout` sets another
default, from 1 to 1440 minutes (a day), or `0` for a store where every
command asks:

```sh
apprafter lock on --timeout 15
```

On a store that is already locked, the same command changes the default,
after asking for the lock password.

If `lock on` is interrupted, run it again: once it has written the vault, it
finishes by asking for both the lock password and the recovery key it
showed. Without one of them, `apprafter lock off` backs out with the other,
and a new `apprafter lock on` starts over with a new recovery key.

## Work with a locked store {#unlock}

```sh
apprafter unlock
```

It asks for the lock password, or the recovery key, and opens an **unlock
window** in this terminal. Until it ends, commands started from this terminal
use the store's secrets without asking. A command run without `unlock` asks
for itself, and its answer opens the window too.

- The window lasts the store's default, or the minutes
  `apprafter unlock --timeout 30` gives it, and each command that uses it
  starts it again. There is no other limit on its length. `apprafter doctor`
  and `apprafter whoami` use it when they ping — they read the token through
  it — and doctor's Kubernetes API check when it reads the cached
  kubeconfig; `apprafter doctor --no-ping` reads no token through it.
- It ends when its time runs out, when the terminal closes or its shell
  exits, when the computer sleeps, when the store's lock is turned off, and
  with `apprafter lock now`.
- **Everything started from that terminal can use it, an AI agent
  included.** Unlock in a terminal no agent runs in, or give the store a
  window of `0`.
- Four things ask for the lock password even inside a window, because they
  change the store rather than read it: adding, renewing, renaming or
  removing a target (`target add`, `target add --renew`, `target rename`,
  `target remove`, `import`), `apprafter unlock` itself, changing the default
  window with `apprafter lock on --timeout <minutes>`, and `apprafter lock off`.
  An agent inside your window can use the store's secrets while it lasts, but
  cannot add a target, replace a token, lengthen future windows or remove the
  lock.
- **Answer a prompt only for a command you started.** It names the command
  and the program that started it. If it names one you did not run — an
  agent's tool, say — press Ctrl-C: answering would open the window for it.
- `apprafter lock status` says whether the store is locked, its slots, the
  window left in this terminal and what this computer lets other programs do.
- A computer that hibernates writes its memory to disk, an open window's key
  included. End the window with `apprafter lock now` first.

A kubeconfig or Argo CD password you print with `apprafter kubeconfig` or
`apprafter argocd-password` and save to a file is outside the lock from then
on; the CLI says so when its output does not go to a terminal.

## Give an agent a store of its own {#playground}

The lock is per target store, so an agent can have a store of its own: keep
the stores that hold real targets locked, and give the agent a separate
store, in files, for a test or experimental target it may drive freely.
This is the recommended way to let an agent work with AppRafter.

Make the agent an SSH key of its own:

```sh
ssh-keygen -t ed25519 -f ~/.ssh/agent_ed25519
```

Start the agent with these variables in its environment — its launcher's,
not your own shell's:

```sh
export APPRAFTER_CONFIG_DIR="$HOME/.config/apprafter-agent"
export APPRAFTER_AGE_KEY="$HOME/.config/apprafter-agent/age.key"
export APPRAFTER_SSH_PRIVATE_KEY="$HOME/.ssh/agent_ed25519"
```

and, at a terminal of your own, register its target in that store with a
token of its own — `target add` asks for it without showing it:

```sh
APPRAFTER_CONFIG_DIR="$HOME/.config/apprafter-agent" apprafter target add sandbox \
    --provider hetzner-cloud --ssh-key ~/.ssh/agent_ed25519.pub
```

- **A Hetzner project of its own.** A token reaches its whole project, and
  `apprafter destroy` empties it — see [the destroy
  scope](target-store.md#destroy-scope). Put nothing in that project you
  would not let the agent run or delete.
- **Its own age key.** `APPRAFTER_AGE_KEY` keeps its caches under a key no
  other store shares.
- **Its own SSH key.** The key that reaches its node should not reach
  yours.
- **Never lock it.** Its secrets are in files, readable by the agent by
  design. `HCLOUD_TOKEN` works there, so a CI job can use the same layout.

## A boundary for the agent {#a-boundary-for-the-agent}

Every program you run as yourself shares one trust domain: the lock stops an
agent that oversteps, not one that sets out to get past it. For that, run the
agent where it cannot reach your terminals, your files or your sessions:

- **A separate operating-system account** for the agent, with its own home
  and its own target store — the layout above. The vault, the credentials
  files and the state files are readable only by your account, so an
  account without administrator rights reads none of them. Do not give the
  agent's account administrator rights, `sudo`, or membership of `docker` or
  of your own groups — on macOS and Windows, make it a Standard account.
  Start the agent in a login of its own, not from a terminal you use, and
  never type a lock password in a terminal the agent's account has used.
- **A sandbox or a container** that mounts only the agent's own store and
  project.
- **Your ssh-agent out of its reach.** A program that can talk to your
  `SSH_AUTH_SOCK` can use every key loaded in it.
- **Your terminal multiplexer out of its reach.** tmux, screen, zellij,
  WezTerm and kitty let any program running as you type into their
  terminals, unlocked ones included.

??? note "Creating a separate account"
    On Linux, as an administrator: `sudo useradd --create-home agent`, then
    start the agent in a login of its own — for example `ssh agent@localhost`,
    or a separate desktop login — not from a terminal you use. On macOS:
    System Settings › Users & Groups › Add User, as a Standard account. On
    Windows: Settings › Accounts › Other users, as a Standard account. Do not
    add the account to `sudo`, `wheel`, `admin`, `docker` or your own groups.

## The SSH key that reaches your nodes {#ssh-key}

`apprafter kubeconfig --refresh` fetches a kubeconfig from the node over SSH
with the key `APPRAFTER_SSH_PRIVATE_KEY` names, or `~/.ssh/id_ed25519`. That
key is not in the vault, and it logs in to the node as root: anything that
can use it — or an ssh-agent holding it — has root on the node, and so the
cluster, lock or no lock. For targets an agent must not reach:

- give the key a passphrase:

    ```sh
    ssh-keygen -p -f ~/.ssh/id_ed25519
    ```

- use a key dedicated to those targets' nodes, and keep it out of any
  ssh-agent an AI agent can reach.

`apprafter lock status`, and `apprafter doctor` on a locked store, warn when
that key has no passphrase or an ssh-agent holds it, or when an ssh-agent
does not answer whether it does.

## Stop locking a store {#lock-off}

```sh
apprafter lock off
```

It shows what goes back where and warns that the secrets will be on disk in
clear again, then asks for the lock password or the recovery key — even
inside an unlock window. It ends this terminal's unlock window, writes each
token back to its target's `credentials.yaml`, re-encrypts any cached
secrets to the shared age key — `~/.config/apprafter/age.key`, or the file
`APPRAFTER_AGE_KEY` names, created if there is none; a store with no cached
secret gets no key file — and deletes the vault. Windows open in other
terminals end within about five seconds.

`lock off` refuses while a target's `credentials.yaml` holds a token in clear
on the locked store: renew that token first — at a terminal,
`apprafter target add <name> --renew`, which asks for it without showing it
([`token_outside_vault`](troubleshooting.md#token-outside-vault)). If
`lock off` is interrupted, run it again.

## Back it up {#backup}

- Back up the store's `vault` and `store.yaml` together, with the rest of the
  target store. Both can go in your backups: the vault is encrypted, and a
  copy is as strong as your lock password, since it can be attacked
  offline; `store.yaml` holds no secret.
- Keep the recovery key apart from that backup, and off this computer.
- A restored store opens with the lock password or the recovery key it had
  when the backup was made.

## What doctor and lock status show {#what-doctor-and-lock-status-show}

```sh
apprafter lock status
apprafter doctor
```

```text
Target store:   ~/.config/apprafter (id 3f9c0a1b7d2e4c56; targets: prod, staging)
Secrets:        locked — in the vault, opened by a password or the recovery key
Slots:          lock password · recovery key
This terminal:  unlocked, 3 min left (a window lasts 5 min, restarted by each command that uses it)
This computer:
  ⚠ The SSH key ~/.ssh/id_ed25519 that reaches your nodes has no passphrase
  ℹ Everything started from this terminal can use its window, an AI agent included
  ℹ While a command talks to a cluster, its kubeconfig is a file programs running as you can read
  ℹ A program running as you that sets out to get past the lock can read what you type, change what you run or use an unlocked window; run AI agents under a separate OS account or in a sandbox
  ℹ Hibernation writes memory to disk, an open window's key included
The lock protects secrets at rest. While a command or the app uses one, it is in that process's memory and, for a kubeconfig, in a file programs running as you can read; the SSH key that fetches kubeconfigs stays outside the lock.
```

What each line means. `apprafter lock status` lists the limits of this
computer always; `apprafter doctor` lists them, and the facts beside the
store, only when the store is locked or a `lock on` did not finish:

| Line | Shown when | What to do |
| --- | --- | --- |
| Secrets in files | the store is not locked | `apprafter lock on` |
| Secrets locked | the store is locked | — its slots, and this terminal's window |
| This terminal's window is open, but its holder does not answer | a window's holder stopped answering; it keeps the store's key until it ends | `apprafter lock now`, which ends it or names the process to stop |
| Secrets locked, but the vault cannot be read | the vault is missing, damaged, from a newer AppRafter, or another store's | [`vault_unreadable`](troubleshooting.md#vault-unreadable) |
| Secrets: whether they are locked cannot be read | `store.yaml` cannot be read | [`store_file_unreadable`](troubleshooting.md#store-file-unreadable) |
| Secrets: turning the lock on (or off) did not finish | `lock on` or `lock off` was interrupted | run it again |
| Token and cluster rows skipped: "the target store is locked" | doctor, with no unlock window open in this terminal | `apprafter unlock`, then `apprafter doctor` again |
| A target's credentials file holds a token in clear, or names another store's vault | doctor, beside a locked store | renew that target's token, as the row says |
| A target's state file holds a kubeconfig in clear | doctor, beside a locked store | `apprafter kubeconfig --refresh --target <name>` |
| The slot list in store.yaml differs; a slot uses key-derivation settings this AppRafter refuses; the vault has no recovery-key slot; the vault file's mode or owner | doctor, beside a locked store | as the row says |
| `HCLOUD_TOKEN` or `APPRAFTER_AGE_KEY` is set | doctor, beside a locked store | unset it, or give that work a store of its own |
| A target's credentials or state file, or the age key, that other accounts may open | doctor, on any store, when true | as the row says ([troubleshooting](troubleshooting.md#secret-file-wide)) |
| A folder of the target store another account owns or can write, or a secret file another account owns | doctor, on any store, when true | as the row says ([troubleshooting](troubleshooting.md#store-folder-foreign)) |
| The SSH key … has no passphrase, or is loaded in ssh-agent | when true, or when the ssh-agent does not answer | [The SSH key that reaches your nodes](#ssh-key) |
| A kubeconfig in use is a file | always | — what the lock does not cover |
| A program running as you that sets out to get past the lock | always | [A boundary for the agent](#a-boundary-for-the-agent) |
| Hibernation writes memory to disk | always | `apprafter lock now` before hibernating |
| X11 session, or Xwayland | Linux, when true | a Wayland session with a Wayland-native terminal |
| Yama `ptrace_scope` is 0, or no Yama | Linux, when true | set the `kernel.yama.ptrace_scope` sysctl to 1 |
| This terminal runs inside a multiplexer | when true | unlock in a terminal outside it |
| Everything started from this terminal can use its window | a window is open here | start agents elsewhere, or `apprafter lock now` |
| Unlock windows on Windows are open to every program running as you | Windows | unlock in a console of its own, and keep windows short |
| A program with debugging rights can read an unlocked window's memory | macOS | run agents under another Standard account |
| The unlock window's key is less protected than usual | the system refused one of its protections | — the window works without it |

## In the desktop app {#desktop}

AppRafter Desktop has one **Lock** section in Settings: the app's own lock and
the target store's lock are one mechanism.

- **Protect…** turns the lock on in five steps: what it does, the lock
  password typed twice (15 characters or more is recommended; a shorter one is
  accepted with a warning), the recovery key shown once, the key typed back in
  full from where you saved it, and the run — which can be cancelled until it
  starts re-encrypting the cached secrets. The app's **Copy** is the only way
  to copy the recovery key: it puts the key on the clipboard marked to stay out
  of clipboard history (on Windows, out of the cloud clipboard as well) and
  clears it after 60 seconds, or sooner when the app locks or quits; on a Mac,
  Handoff does not carry it to your other devices. Choosing **I saved it**
  takes the key off the clipboard and ends the copies, so the key you type
  back comes from where you saved it.
- While the store's secrets are protected, unlocking the app opens them: the
  lock screen asks for the lock password or the recovery key on every operating
  system. Windows Hello, Touch ID and your system password cannot open the
  vault yet. While the app is unlocked it holds the store's key. Locking it,
  the idle timeout, quitting, and the computer sleeping or its screen locking
  where the computer tells the app, end that: from then on the app starts no
  new read of a secret. A read already decrypting ends, and an operation you
  confirmed keeps what it already read but stops at its next secret. A sleep
  the computer does not report still locks the app: it notices the sleep
  from its own clocks when the computer wakes, before any secret is used. A
  change you confirmed before the lock — protecting, finishing, stopping
  protection, or saving, renaming or removing a token — finishes with the
  key it took once no other AppRafter program holds the store, or is
  abandoned with nothing written if it cannot start within 30 seconds; that
  key goes when the change ends.
- Meanwhile *Require unlock*, *Lock when the app starts* and *Lock when the
  computer sleeps or locks* stay on, and *Auto-lock after inactivity* offers no
  *Never*. Your own choices are kept, and come back when you stop protecting.
- **Stop protecting…** asks for the lock password or the recovery key, even
  though the app is unlocked, and puts the Hetzner tokens back in their files.
  If the store has cached secrets, it re-encrypts them to the shared age key
  in its default place, `~/.config/apprafter/age.key`, creating that file
  only then; a store without cached secrets never touches it. The app does
  not read `APPRAFTER_AGE_KEY`: to put cached secrets under another key, run
  `apprafter lock off` in a terminal.
- After an interrupted run the section offers **Finish…**. An interrupted
  Protect that wrote the vault asks for both the lock password and the
  recovery key; one that did not starts over with a new recovery key, or
  **Stop protecting…** backs it out with a plain confirmation, since nothing is
  encrypted yet.
- If the target store's `store.yaml` cannot be read, or its vault is missing,
  the app says so — on the lock screen and in Settings — with how to repair
  it, and never calls the secrets encrypted.
- The section lists what opens the vault and this computer's limits — the
  lines `apprafter lock status` shows — including any protection the app's own
  process could not take, which Doctor shows too.

While the app is unlocked, everything it runs can use the store's secrets:
lock it when you step away. Terminals unlock on their own, with
`apprafter unlock`.

## See also

- [How the store lock works](../how-it-works/the-store-lock.md) — the vault,
  the slots, the unlock window and every route the lock does not close.
- [Managing targets](target-store.md) — the target store's other commands.
- [Environment variables](../reference/environment.md) — the variables a
  locked store refuses, and the ones the unlock window reads.
- [Troubleshooting](troubleshooting.md#store-locked) — every refusal of a
  locked store, and what to do.
