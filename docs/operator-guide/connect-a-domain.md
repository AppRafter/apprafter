---
description: "The once-per-cluster preparation and the per-zone steps that put an application behind Cloudflare on HTTPS."
---

# Connect a domain (public ingress)

This is the end-to-end runbook for serving an application on a public domain
through Cloudflare. You do the cluster prep once, then repeat the per-zone steps
for each registrable domain.

The result: each registered zone is served over HTTPS through Cloudflare's edge,
TLS terminates at the cluster Gateway on a Cloudflare Origin CA certificate, and
the node's IP is firewalled so the only way in is through Cloudflare.

What each step puts in the cluster — the listener pair a zone creates, the route
an application's `expose` block becomes, and what the `PublicRouteReady` verdict
is telling you — is [The public route](../how-it-works/the-public-route.md).

**Prerequisites:** a bootstrapped cluster, the `apprafter` CLI pointed at it
(`apprafter target list` shows it active), and a domain you can change
nameservers on.

## 1. Cluster prep (once)

Restrict the node's `80`/`443` to Cloudflare's IP ranges so an attacker can't
bypass Cloudflare by hitting the node directly:

```bash
apprafter target firewall cloudflare-origin enable
```

It's a cluster-wide setting — do it once, not per zone — and it survives
re-provisioning. Run `apprafter target firewall cloudflare-origin disable` to
reopen `80`/`443`. What the restriction buys, and what it does not, is
[The public route](../how-it-works/the-public-route.md#what-the-origin-firewall-buys-and-what-it-does-not).

> Infrastructure-as-code / fork users can instead opt in via the manifest:
> `spec: firewall: cloudflareOrigin: true` + `apprafter apply` (a manifest value
> overrides the CLI toggle).

## 2. Per zone (repeat for each domain)

### 2.1 Point the domain at Cloudflare

1. In the Cloudflare dashboard, **Add a site** for `<zone>` (the Free plan is
   enough). Note the two nameservers Cloudflare assigns.
2. At your domain registrar: disable DNSSEC, then set the nameservers to
   Cloudflare's two.
3. Back in Cloudflare, wait for the zone to become **Active** (the nameserver
   change can take a while to propagate).

### 2.2 DNS records

You need the node's public IP. Run:

```bash
apprafter target ip
```

It prints the `A` (IPv4) and `AAAA` (IPv6) record values from the Hetzner API.
(Fallback: `kubectl get nodes -o wide` shows them too — but on Hetzner, which has
no cloud-controller-manager, the public IP appears under the **`INTERNAL-IP`**
column, not `EXTERNAL-IP`.)

In Cloudflare's DNS for the zone, add (all **Proxied** — orange cloud):

| Type  | Name | Value          |
|-------|------|----------------|
| A     | `@`  | `<node-IPv4>`  |
| AAAA  | `@`  | `<node-IPv6>`  |
| CNAME | `www`| `<zone>`       |

The `www` record is optional.

### 2.3 TLS mode + origin certificate

1. **SSL/TLS → Overview**: set the mode to **Full (strict)** so Cloudflare
   validates the origin certificate.
2. Mint a **Cloudflare Origin CA** certificate for `<zone>` + `*.<zone>` and
   import it — the full steps are in
   [Cloudflare Origin CA certificate](cloudflare-origin-cert.md). In short:

   ```bash
   apprafter target cert import cf-origin-cert-<sanitized-zone> \
     --cert ./origin.pem --key ./origin.key
   ```

   (`<sanitized-zone>` is the zone with dots turned into dashes, e.g.
   `cf-origin-cert-apprafter-dev` for `apprafter.dev`.)

### 2.4 Register the zone

```bash
apprafter target domain add <zone> --cert cf-origin-cert-<sanitized-zone>
```

The Gateway gains an apex + wildcard `:443` listener pair for the zone, both
terminating TLS from the imported certificate
([why two](../how-it-works/the-public-route.md#what-registering-a-zone-creates)).
The command then points you at `apprafter target ip` for the DNS record values
above.

### 2.5 Expose an application on the domain

In an application manifest, set the hostname and make it public:

```cue
spec: base: expose: {
    port:     8080
    network:  "public"
    hostname: "<zone>"          // or a subdomain, e.g. "app.<zone>"
}
```

Apply the application as usual —
[the operator turns that block into a route on the Gateway](../how-it-works/the-public-route.md#how-an-expose-block-becomes-a-route).

!!! warning "This edit waits for an approval"

    Making an application public, and giving a public application its first
    hostname, are both changes the platform holds rather than rolling out — an
    address answering as you is worth a second pair of eyes. The application
    keeps serving whatever it served before, and `apprafter app status` reports
    `AwaitingMigrationApproval`. You are the person who can release it:

    ```sh
    apprafter migration list
    apprafter migration approve <plan>
    ```

    What else holds, and why, is [When a change needs
    approval](../dev-guide/when-a-change-needs-approval.md).

One that is live, if you want the finished shape: this documentation site is
served from `docs.apprafter.dev`, a subdomain of an already-registered zone, so
it needed no new zone and no new certificate — and its `expose` block is
exactly the three fields above
([worked example](../how-it-works/deploying-the-docs-site.md)).
[The landing site and its CMS](../how-it-works/deploying-the-landing-and-cms.md)
is the two-host version of the same thing: the apex on one application and
`cms.` on another.

## 3. Verify

- **Per zone** — the page loads through Cloudflare with a Cloudflare edge
  certificate, served by your application:

  ```bash
  curl -v https://<zone>/
  ```

- **Cross-zone** — if you registered more than one zone on the cluster, each
  serves its own application and presents its own certificate at the edge:

  ```bash
  curl -sI https://<zone-a>/ ; curl -sI https://<zone-b>/
  ```

- **The origin firewall blocks a bypass** — hitting the node directly, skipping
  Cloudflare, is refused (or times out):

  ```bash
  curl --resolve <zone>:443:<node-ip> https://<zone>/
  ```

- **List the registered zones**, with a count of the applications using each:

  ```bash
  apprafter target domain list
  ```

## Removing a zone

```bash
apprafter target domain remove <zone>
```

`remove` is blocked while applications still reference the zone (re-point or
remove those apps first, or pass `--force`). It leaves the imported certificate
Secret in place — remove that separately if it is no longer used.
