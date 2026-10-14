---
description: "What apprafter lock on changes in a target store: the vault and its slots, the store's own age key, how a locked store is opened, the per-terminal unlock window and what binds it, where a secret is while it is used, and what the lock does not stop."
---

# How the store lock works

What `apprafter lock on` changes in a target store, how a locked store's
secrets are opened, and where the lock stops. The recipe is [Protecting
secrets from agents](../operator-guide/protecting-secrets-from-agents.md);
none of this is needed to use it.

Read it when you are deciding whether the lock answers the threat you have
in mind, when an unlock window behaved in a way you did not expect, or when
you are backing up or moving a locked store.

The decision is [ADR 0068](../adr/0068-target-store-lock.md): an opt-in lock
per target store, with no operating-system keyring behind it. It stops an AI
agent that oversteps, and only raises the bar for a program running as you
that sets out to get past it.

## What a locked store keeps where {#layout}

```text
<store>/
├── store.yaml            # whether the secrets are locked; the store's id; the slot list, for display
├── vault                 # mode 0600: the tokens and the store's own age key, encrypted
├── targets/<target>/
│   ├── config.yaml       # unchanged
│   └── credentials.yaml  # a marker naming the vault, no token
└── state/<target>/.apprafter/
    └── state.json        # the cached kubeconfig and Argo CD password, encrypted to the store's key
```

**`store.yaml`** says where the secrets are: `secrets: files`, `locked`, or
`locking` / `unlocking` for a `lock on` or `lock off` that did not finish. A
store without it keeps its secrets in files. It also holds the store's id,
which the vault's header must name too, and copies of the slot list and the
default unlock window, for display: the vault's own are the ones that count,
so editing `store.yaml` cannot add a slot or lengthen a window. Every command
reads it again, so a `lock on` in one terminal is seen by the next command in
another. If it cannot be read, the store is treated as locked and none of its
secrets is used. `cli/apprafter-core/src/secrets/store_file.rs`.

**The vault** begins with a header naming its format, the store's id and its
slots. Then, encrypted with XChaCha20-Poly1305 under a random 256-bit data
key, it holds every target's Hetzner token, the store's **own age identity** —
made by `lock on`, used by this store alone — and the default unlock window.
Everything before the encrypted part is authenticated with it, so a change to
the header is detected. The format is frozen by test vectors: a released
version never changes meaning. `cli/apprafter-core/src/vault/format.rs`,
`cli/apprafter-core/src/vault/seal.rs`.

**A target's `credentials.yaml`** holds a marker where the token was:

```yaml
# The Hetzner API token of this target is in this target store's vault (`apprafter lock on`).
# AppRafter releases without `apprafter lock` cannot read this file: update AppRafter.
hetzner_token:
  vault: 3f9c0a1b7d2e4c56
```

An older AppRafter stops at it rather than reading it as a token. A token
found in clear in a locked store's credentials file — written by an older
release or another program — is never used: it cannot be told from a
planted one. `cli/apprafter-core/src/secrets/marker.rs`.

**The caches** stay in `state.json`, encrypted to the store's own age
identity. The identity's public half is derived from the vault whenever a
cache is written and is stored nowhere else, so a program that can edit the
store's files cannot make the store trust a cache encrypted to a key of its
own. A cache in clear, or one the store's key does not open, is refused: fetch
it again from its source. The shared `~/.config/apprafter/age.key` stays
where it was, for the stores that are not locked.

**Who else can change these files.** The lock relies on the store being
your account's alone: another account that owns one of its folders, or can
write to one, could put a `store.yaml`, a vault or a target's files of its
own there. Such a store is refused before any of its secrets is used or
written, locked or not, and `apprafter lock on` and `apprafter lock off`
refuse it before they ask for a password
([`store_not_private`](../operator-guide/troubleshooting.md#store-not-private)),
and `apprafter doctor` names each folder concerned
([The target store on disk](the-target-store.md#file-layout)).

## Slots {#slots}

A slot is one way to open the vault: the data key, wrapped with a key derived
from the slot's secret.

- **The lock password.** Argon2id derives the wrapping key, with its
  parameters stored in the slot: 256 MiB of memory, three passes and four
  lanes by default. A release build refuses parameters below its floor, both
  when it creates a slot and when it opens one. There is no minimum length;
  `lock on` warns below 15 characters.
- **The recovery key.** 32 random bytes, shown as text beginning
  `APPRAFTER-RECOVERY-KEY-1`, then eleven groups of five characters and one
  of three. It goes through no Argon2: its 256 random bits are what make it
  hard to guess. Its checksum turns a mistyped character into a message —
  naming the group, where it can — never a wrong key.

Every slot first checks a key commitment, so a wrong secret is told apart
from a damaged slot before anything is decrypted; a failed decryption is
always an error, never "no such secret". A slot of a kind this release does
not know is kept, byte for byte, when the vault is written again.
`cli/apprafter-core/src/vault/kdf.rs`,
`cli/apprafter-core/src/vault/recovery.rs`.

## Turning the lock on and off {#on-and-off}

`lock on` runs in steps, each resumable
(`cli/apprafter-core/src/lock/on.rs`). It writes `secrets: locking`; writes
the vault without replacing any file of that name, and flushes it to disk;
reads it back and opens it with **both** slots; re-encrypts each cache to the
store's key; replaces each token with the marker, but only where the clear
token equals the vault's; and writes `secrets: locked`. No copy in clear is
removed before the vault has been written, read back and opened. Run again
after an interruption, it finishes — asking for both secrets — and `lock off`
backs out of it instead.

`lock off` (`cli/apprafter-core/src/lock/off.rs`) writes
`secrets: unlocking`, puts each token back into its credentials file,
re-encrypts each cache to the shared age key — `APPRAFTER_AGE_KEY` names
where it goes, when set — checks every file, deletes the vault and writes
`secrets: files`. A clear token that differs from the vault's, written while
a run was interrupted, is kept: it is the newer one. The shared key is read,
or created, only when there is a cache to move: on a store with none,
neither `lock on` nor `lock off` touches it, and neither plan names it.

Every write to the vault takes the store's write lock and reads
`store.yaml` again under it: a write prepared before another process turned
the lock on or off is refused rather than applied to the wrong store.

## Opening a locked store {#opening}

Every read of a token or a cache goes through one door
(`cli/apprafter-core/src/secrets/mod.rs`), which asks the client how to open
a locked store. In the CLI (`cli/platform-cli/src/vault_access.rs`), in this
order:

1. what this command already opened;
2. this command's own key, if it asked for one itself — a command that
   prompted never reads through a terminal's window afterwards;
3. this terminal's unlock window;
4. a prompt at the command's **controlling terminal**
   (`cli/platform-cli/src/prompt/secret.rs`) — never stdin or stderr, so
   `2>/dev/null` cannot hide it and a pipe cannot answer it. It names the
   store, the command and the program that started it, so you can tell your
   own command from one an agent ran in your terminal. Three wrong answers end
   the command; five minutes without one is `store_locked`;
5. otherwise `store_locked`.

`doctor`, `whoami`, `status`'s `secrets:` line, `target show`, `target list`
and `lock status` never ask: they read through an open window, or report the
store as locked. Reading through a window is using it: every item the
holder serves starts the window's time again, a diagnostic's as any
command's. `whoami` and `doctor` use it when they ping — they read the
token through it — and `doctor`'s Kubernetes API check when it opens the
cached kubeconfig; `doctor --no-ping` reads no token through it.

A write to the vault — adding, renewing, renaming or removing a target,
`import`, changing the default window, `lock off` — never uses a window: the
command asks, and uses the key it derived itself.

`HCLOUD_TOKEN` and `APPRAFTER_AGE_KEY` are refused on a locked store before
anything is asked: a value from the environment would pass the lock
unnoticed.

## The unlock window {#window}

`apprafter unlock`, or a command's own prompt, starts a **holder**: the
`apprafter` binary run as a background process
(`cli/platform-cli/src/holder/mod.rs`). It receives the data key once, over a
pipe from the command that started it, keeps it in one memory page — locked
against paging, where the system allows — and gives it to nobody: it answers
one request at a time — a token, a decrypted cache, the store's public
recipient — and never writes the vault.

It protects itself where the system allows, and reports each measure that
failed (`apprafter lock status`, and a doctor row): not dumpable on Linux, no
core files on Linux and macOS, no debugger attach on macOS, a process access
list that admits only what it needs on Windows
(`cli/platform-cli/src/holder/harden.rs`). On Windows it always reports that a
crash may write its key to disk: it turns the crash dialog off, but Windows
Error Reporting may still write a dump.

**Who it serves** (`cli/platform-cli/src/holder/binding.rs`). On Linux and
macOS: a process of the same user in the same session as the shell where the
secret was typed — that terminal's session — while the session's leader is
still the same process. On Windows: a process attached to the same console
(in Windows Terminal, the same pane). **On Windows any program running as you
can attach itself to that console, and is then served**: there the window is
open to every program of yours while it lasts.

**Where it listens.** A socket in `$XDG_RUNTIME_DIR/apprafter/`, in a
directory only you can open; without `XDG_RUNTIME_DIR`, the store's own
`run/` on Linux and `$TMPDIR/apprafter/` on macOS. On Windows, a named pipe
that admits only your logon session. These keep out other users without
administrator rights; they do not keep out your own programs.

**When it ends** (`cli/platform-cli/src/holder/window.rs`): after the
store's window of minutes without a request served — each one starts it
again, and there is no other limit on its length — and also with
`apprafter lock now`, when the terminal's session ends, when the computer
sleeps (a clock that counts sleep runs ahead of one that does not), and within
about five seconds of the store no longer being locked under the same id
(`lock off`, or a deleted or replaced `store.yaml`). A window of `0` minutes
starts no holder. A computer that hibernates writes the page to disk with the
rest of memory; `apprafter doctor` and `apprafter lock status` say so on a
locked store.

## Where a secret is while it is used {#in-use}

The lock protects secrets at rest. While a command or the app uses a secret,
it is in that process's memory. Beyond that:

- **A kubeconfig handed to `kubectl` or `helm`** is written, decrypted, to a
  file in the runtime directory (`<store>/run`; on Windows with
  `APPRAFTER_CONFIG_DIR` unset, under `%LOCALAPPDATA%\apprafter\run`), for
  as long as the command runs, and removed when it ends; a file whose
  process is gone is removed by the next command that writes one. The file
  and the directory are your account's alone wherever the store is: mode
  0600 in a 0700 directory on Linux and macOS; on Windows, a DACL that
  admits only your account and takes nothing from the folders above, set
  before the first byte is written. Other accounts without administrator
  rights cannot read it; programs running as you can, meanwhile. A runtime
  directory another account owns is refused: on Linux and macOS its mode
  cannot be set; on Windows AppRafter checks its owner, who could give
  itself access again and replace the kubeconfig before `kubectl` reads it
  ([`runtime_dir_foreign`](../operator-guide/troubleshooting.md#runtime-dir-foreign)).
  The cleanup a command runs after Ctrl-C uses a short-lived copy in the
  system's temporary directory, your account's alone in the same way from
  its first byte, whatever that directory lets other accounts do; so are
  the manifests a command hands to `kubectl apply` or `helm` there, and the
  directories a backup stages its export in and a restore unpacks into.
  `kubectl` still reads the copy while the command holds it.
  `cli/apprafter-core/src/runtime.rs`, `cli/cli-core/src/owner_only.rs`.
- **`apprafter open argocd`** puts the Argo CD password on the clipboard,
  which programs running as you can read, and forwards a local port any
  program on this computer can connect to.
- **What you save** from `apprafter kubeconfig` or `apprafter argocd-password`
  stays where you saved it.
- **The SSH key** that fetches a kubeconfig from the node
  (`APPRAFTER_SSH_PRIVATE_KEY`, or `~/.ssh/id_ed25519`) is not in the vault,
  and it logs in to the node as root: anything that can use it — or an
  ssh-agent holding it — has root on the node, and so the cluster, lock or
  no lock.

## What the lock does not stop {#limits}

Programs that run as you share one trust domain. The lock stops one that
oversteps; against one that sets out to get past it, the defence is an
operating-system boundary — another account without administrator rights,
or a sandbox. What such a program can still do, and what shows it on this
computer:

| Route | What shows it |
| --- | --- |
| Read the lock password as you type it — by opening your terminal device and racing the prompt, or, under X11 or in a terminal running under Xwayland, by reading keystrokes | the X11 / Xwayland row of `apprafter doctor` and `apprafter lock status`; their row for programs that set out to get past the lock |
| Change what you run: your shell's startup files, `PATH`, a binary in a directory you can write | the row for programs that set out to get past the lock |
| Type into an unlocked terminal through a multiplexer's own commands (tmux, screen, zellij, WezTerm, kitty) | the terminal multiplexer row |
| Attach a debugger to your shell (Yama `ptrace_scope` 0, or no Yama) | the `ptrace_scope` row |
| On Windows, attach to the unlocked console; on macOS, with debugging rights, read the holder's memory | the Windows and macOS rows |
| Take the holder's socket path before it does: a client believes its replies, so it could hand out a forged token or kubeconfig. On macOS and Windows the client checks that the program behind the socket or pipe is this `apprafter`; AppRafter makes no such check on Linux | the row for programs that set out to get past the lock |
| Copy the vault and guess a short lock password offline | the warning `apprafter lock on` gives below 15 characters |
| Run an older AppRafter on a locked store: it stops at the markers, but could still store a new target's token, or a kubeconfig, in clear — which a newer release then refuses to use | `apprafter doctor`: the target's token or kubeconfig row, and a warning beside a locked store for any other target ([`token_outside_vault`](../operator-guide/troubleshooting.md#token-outside-vault)) |

Two further limits are stated rather than shown: the key derivation's
intermediate hashing state is not wiped from memory (the derived keys are),
and a cache an older AppRafter encrypted to the shared age key looks like any
other until it is opened — it is refused then, and whoever holds the shared
key can read it until you fetch it again
([`cache_undecryptable`](../operator-guide/troubleshooting.md#cache-undecryptable)).

## What the reports read {#reports}

`apprafter doctor`, `apprafter status` and `apprafter lock status` read one
report (`cli/apprafter-core/src/lock/status.rs`): `store.yaml`, the vault's
header — parsed, never opened — and this terminal's window, asked of its
holder without starting it again. `doctor` and `lock status` also read what
only the client can see: the terminal's multiplexer and display, and whether
the ssh-agent at `SSH_AUTH_SOCK` holds the fetch key (asked within two
seconds; "unknown" past that). To tell whether that key has a passphrase they
read its file — at most 16 KiB, more than any key OpenSSH writes — into a
buffer that is wiped afterwards, and look only at its cipher name and its
public half; nothing of it is kept or shown. None of them asks for a secret.
`apprafter lock status` lists the limits of this computer always;
`apprafter doctor` lists them for a locked store, one being locked, or one
whose `store.yaml` cannot be read (which it treats as locked) — a store in
files has nothing for them to qualify. `cli/apprafter-core/src/lock/limits.rs`,
`cli/apprafter-core/src/doctor.rs`.

## See also

- [The target store on disk](the-target-store.md) — the store's other files,
  and the order a credential is resolved in.
- [Environment variables](../reference/environment.md) — the variables the
  lock refuses, and the ones the unlock window reads.
- [Troubleshooting](../operator-guide/troubleshooting.md#store-locked) — every
  refusal a locked store can give, and what to do.
