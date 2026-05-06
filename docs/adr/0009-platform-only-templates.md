# ADR 0009: Platform-only notification templates

## Status

`Accepted`. Date: 2026-05-06.

## Context

Once the platform ships a notifications service (ADR 0008), there is
a clear temptation to ship a content-template library: welcome
emails, password resets, marketing newsletters, etc. This would put
the platform in competition with SendGrid, Mailgun, ConvertKit, and
similar dedicated tools — a losing battle and a major scope inflator.

## Decision

The platform ships **only** templates needed by the platform itself.
All other templates are owned by applications, stored in their own
Git repositories, and rendered by the same notifications HTTP API.

The platform's built-in templates are limited to:

- AccessGrant lifecycle (issued, renewal-reminder, expired, revoked).
- Operational alerts (DLQ stuck, service down, quota exceeded,
  MigrationPlan pending approval, backup digest).
- Bootstrap (cluster initialised).

Templates can be overridden by deploying a configmap; there is no
template-management UI.

## Consequences

Positive:

- The platform's job remains "transport with rich audit", not
  "marketing tool".
- Scope is contained; we do not become a SendGrid clone.
- Application templates live next to application code, where they
  belong.

Negative:

- Users who expected a template marketplace must look elsewhere.
  Acceptable: docs are explicit about this boundary.

## Alternatives considered

- **Generic template marketplace.** Rejected: out of scope.
- **App-defined templates served by the platform.** Viable as a
  future enhancement, but not v1.0.

## Risks

- Users may file feature requests for built-in welcome emails. We
  redirect them to the HTTP API + their own template files.

## Owner

Notifications maintainers.

## Re-evaluation

If a clear pattern emerges where multiple teams reimplement the same
template, consider extracting into a community-maintained template
collection (separate from the platform).

## References

- `spec.md` §4.6 ("Platform-shipped templates"), §8 ("Why platform-
  only notification templates").
- ADR 0008 (HTTP-first notifications API).
