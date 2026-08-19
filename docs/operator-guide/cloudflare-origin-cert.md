---
description: "Minting the origin certificate Cloudflare's edge trusts, importing it into the cluster, and rotating it."
---

# Cloudflare Origin CA certificate

AppRafter's public ingress terminates TLS at the cluster Gateway using a
certificate you import. With Cloudflare proxying (orange-cloud) in front, the
simplest origin certificate is a **Cloudflare Origin CA** cert — free, valid for
up to 15 years, and trusted by Cloudflare's edge (use SSL/TLS mode
**Full (strict)**).

## Mint the certificate

1. In the Cloudflare dashboard, select your zone.
2. **SSL/TLS → Origin Server → Create Certificate**.
3. Hostnames: `<zone>` and `*.<zone>` (apex + wildcard).
4. Key type: **RSA (2048)**.
5. Validity: **15 years**.
6. Create, then copy the **Origin Certificate** (PEM) and the **Private Key**
   (PEM) into two files, e.g. `apprafter.dev.crt` and `apprafter.dev.key`.

## Import it into the cluster

```bash
apprafter target cert import cf-origin-cert-apprafter-dev \
  --cert ./apprafter.dev.crt \
  --key ./apprafter.dev.key
```

The command validates the PEM, checks the certificate matches the key, extracts
the SANs, and stores a `kubernetes.io/tls` Secret in `apprafter-system`. Re-run
with `--replace` to rotate the certificate in place.

Then register the domain so the Gateway serves it:

```bash
apprafter target domain add apprafter.dev --cert cf-origin-cert-apprafter-dev
```

## Notes

- One certificate per registrable zone — import once per zone.
- Only RSA keys are supported today (Cloudflare Origin CA is RSA 2048).
- Set Cloudflare SSL/TLS mode to **Full (strict)** so the edge validates the
  origin certificate.
