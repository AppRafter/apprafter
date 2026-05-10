# Recovery and emergency console access

When the AppRafter VM becomes unreachable over SSH (cloud-init
hung, ufw/iptables misconfiguration that survived a previous
release, kernel panic, runaway memory, etc.) — and you need to
look at logs from inside the VM rather than just rebuild from
scratch — you have two options:

1. **Hetzner Rescue Mode** (this page). Slow (~5-minute power
   cycle) but always works.
2. **Tear down and re-apply.** Faster (`destroy --yes` +
   `apply`); the right move when you don't actually need the
   data on the disk and just want a working cluster again.

This page documents option 1. AppRafter VMs are key-only — we
pass an SSH key to Hetzner at server creation, so root has **no
password** and the noVNC web console is useless for emergency
diagnostics. Rescue Mode boots the VM with a separate Linux
image that accepts your SSH key, then mounts the original disk
at `/mnt/disk`.

## When you need this

- `platform-cli kubeconfig` times out on `Connection timed out`
  for >5 minutes after `apply`, but the Hetzner API says the
  server is `running`. Most common cause: cloud-init didn't
  finish, or finished with a misconfiguration that drops every
  inbound packet. (The v0.1.43 ufw initcaps bug was exactly
  this.)
- Pods stuck in `ContainerCreating` for >2 minutes after
  `cluster-bootstrap` and you suspect a CNI / kernel-module
  issue. Look at `dmesg` and Cilium agent logs from the disk.

## Procedure

### 1. Enable Rescue Mode

In the [Hetzner Cloud Console](https://console.hetzner.cloud):

- **Project** → server `platform-1` → **Rescue** tab.
- Linux 64-bit, add your SSH public key (the same one you use
  for AppRafter — keeps your fingerprint stable).
- **Activate Rescue & Power Cycle.** Wait ~30 seconds.

### 2. SSH in

```sh
ssh-keygen -R <public-ip>     # rescue boots with a different host key
ssh root@<public-ip>
```

Rescue mode runs from a ramdisk, so SSH is up immediately and
serves on port 22 regardless of the original VM's firewall
config — that's the whole point.

### 3. Mount the original disk

```sh
lsblk                          # find the right partition
# Hetzner default for ubuntu-24.04: /dev/sda1 is the root fs.
# Some types may show /dev/vda1 instead.

mkdir -p /mnt/disk
mount /dev/sda1 /mnt/disk      # adjust if lsblk shows otherwise

ls /mnt/disk/var/log/          # sanity check: should list cloud-init.log etc.
```

### 4. Triage from the disk

These are the files to look at first:

```sh
echo "=== cloud-init final state ==="
cat /mnt/disk/var/log/cloud-init-output.log | tail -120
grep -nE 'FAIL|ERROR|fatal|denied|refused|exit code' \
    /mnt/disk/var/log/cloud-init-output.log

echo "=== firewall state ==="
# If the v0.1.43+ user_data ran, ufw should NOT exist on disk.
# If anything claims iptables/nftables is enabled, dump it:
ls /mnt/disk/etc/iptables/ 2>/dev/null
cat /mnt/disk/etc/nftables.conf 2>/dev/null | head -50
# CNI configs:
ls -la /mnt/disk/etc/cni/net.d/
cat /mnt/disk/etc/cni/net.d/*.conflist 2>/dev/null | head -40

echo "=== k3s install + service ==="
ls /mnt/disk/var/log/ | grep -i k3s     # often empty — k3s logs to journald
ls /mnt/disk/etc/systemd/system/k3s.service.env
cat /mnt/disk/etc/systemd/system/k3s.service | head -20

echo "=== kernel ring buffer at boot ==="
# /var/log/kern.log carries the same data as live `dmesg` on
# the original kernel session; useful for OOMKills, network
# device errors, etc.
grep -E 'cilium|flannel|nftable|kvm|memory|oom' \
    /mnt/disk/var/log/kern.log | tail -40
```

If you need to chroot in (e.g. to run `journalctl` against the
original journal, or run `ufw status` against the original
state):

```sh
mount --bind /dev  /mnt/disk/dev
mount --bind /proc /mnt/disk/proc
mount --bind /sys  /mnt/disk/sys
mount --bind /run  /mnt/disk/run
chroot /mnt/disk /bin/bash
# inside chroot, journalctl reads the original boot journal
journalctl -b -u k3s --no-pager | tail -50
exit
umount /mnt/disk/run /mnt/disk/sys /mnt/disk/proc /mnt/disk/dev
```

> **Note**: chroot inherits the rescue kernel, not the original
> one. Tools that talk to kernel state (e.g. `iptables-nft`,
> `ufw`) may emit `initcaps` errors — that's expected and not
> the same as the runtime symptom on the original boot. Trust
> only the on-disk artefacts (`/etc/ufw/user.rules`,
> `/etc/cni/net.d/*`, `/var/log/cloud-init-output.log`).

### 5. Decide: fix in place vs. rebuild

| Symptom                                                         | Recommended path                                                                      |
| --------------------------------------------------------------- | ------------------------------------------------------------------------------------- |
| One-shot config error from cloud-init (caught by triage above)  | **Rebuild.** Fix our user_data / Helm values via a patch, push, `destroy + apply`.    |
| Disk full, ephemeral state                                      | **Rebuild.** Tier-1 has no persistent data we need to preserve.                       |
| Genuine workload state on disk we want to preserve              | Edit the file in place from rescue, unmount, disable Rescue Mode, power-cycle back.   |

For tier-1, **rebuild is almost always the right answer** —
provisioning is fast (~3 min) and the bug we just identified
becomes a patch, not a one-off recovery.

### 6. Disable Rescue Mode

```sh
umount /mnt/disk
```

Console → server → **Rescue** → **Disable** → **Power off** +
**Power on**. The VM boots back into the original ubuntu image
on the original kernel. SSH host key reverts to whatever the
original VM had (different from the rescue host key) — clear
the rescue entry from your local `known_hosts` if needed:

```sh
ssh-keygen -R <public-ip>
```

(With v0.1.46+ AppRafter manages a per-cluster
`.apprafter/known_hosts` for the `platform-cli kubeconfig` SSH
step — the rescue/back-to-prod host key swap doesn't trip the
CLI itself; the manual `ssh root@…` you used during rescue is
where you'll see the warning.)

## Why we don't ship an emergency root password

Setting an emergency root password in cloud-init (`chpasswd`)
would make the noVNC console useful without Rescue Mode. We
deliberately chose **not** to do that, for three reasons:

1. **Key-only auth is the secure default for tier-1.** A
   password — even one in `state.json` — is a credential
   surface that grows over time (rotation policy, leakage
   risk, audit). Hetzner Rescue Mode achieves the same outcome
   for genuine emergencies without changing our security
   baseline.
2. **For tier-1 the right answer is almost always rebuild.**
   Time-to-recovery via `destroy + apply` is ~3 minutes;
   chasing a one-off VM bug in noVNC takes longer and ends
   with the same patch we'd write either way.
3. **For tier-3/4 (regulated / confidential) we'll revisit.**
   When those tiers land, the noVNC fallback is the kind of
   knob that goes behind an explicit opt-in env (e.g.
   `APPRAFTER_EMERGENCY_ROOT_PASSWORD`) with audit logging on
   first use, not a default.

If you have a use case that genuinely requires noVNC console
on tier-1, file an issue — we'll discuss adding an opt-in.
