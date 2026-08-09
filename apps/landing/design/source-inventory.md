# Landing source inventory and implementation spec

## Reference and intent

The read-only reference is `private-code/landing`. Its visual language is retained, while every hosted-SaaS, stale package, unverified benchmark, and pricing claim is replaced with the current self-hosted FFDB architecture, six public `@ffdb/*` npm identities, and matching versioned offline tarballs.

## Visual system

- Warm paper background (`oklch(98.5% 0.002 90)`) with near-black ink, white cards, hairline black borders, and muted warm gray text.
- Display typography is a high-contrast serif; body copy is a geometric sans; labels and code use a compact monospace face.
- Section width caps at 1400px. Desktop gutters are 48px, mobile gutters are 24px.
- Buttons are rounded pills. Content panels stay square or use a 4px radius.
- Motion is quiet and structural: character reveals, a rotating ASCII sphere, row/cursor pulses, marquees, hover translation, and intersection-based reveals. Reduced-motion preferences disable animation.
- Background texture uses fine grids, subtle noise, sparse geometric line art, and thin dividers rather than gradients or glossy effects.

## Source component inventory

1. Fixed navigation that becomes a floating translucent rail after scrolling; full-screen mobile menu.
2. Full-viewport hero with eyebrow, oversized two-line serif headline, rotating word, ASCII sphere, primary/secondary pills, and a bottom stats marquee.
3. Four numbered capability rows, each pairing copy with a small animated monochrome diagram.
4. Dark “how it works” section with vertical step tabs and a code window.
5. Split architecture section with narrative/stat labels and a live pipeline list.
6. Four-cell metric grid.
7. Two full-width integration marquees.
8. Security split section with principle tags and feature cards.
9. Developer split section with tabbed, copyable code and a compact benefits grid.
10. Three-column pricing grid.
11. Bordered closing CTA with oversized type and geometric art.
12. Multi-column footer over animated line art.

## Product rewrite map

- Hero: “The self-hosted backend for SQLite” and one project database per project, backed by a PostgreSQL control plane and isolated Rust workers.
- Capabilities: hardened SQLite projects, PostgreSQL-style RLS, built-in auth, logical offline sync, and RLS-protected S3-compatible storage.
- Workflow: install the signed `single-host` evaluation release without a checkout, verify it through `ffdb-host` and `/readyz`, apply a migration with policies, then connect through `@ffdb/client`.
- Architecture: logical changes and opaque cursors, never SQLite WAL frames; server sequence orders conflicts; scope changes can force resnapshot.
- Metrics: only architectural facts (one SQLite DB per project, two credential modes, one logical change stream, zero raw database files exposed). No latency, throughput, uptime, or certification claims.
- Integrations: the six public `@ffdb/*` npm packages and their documented compatible runtimes.
- Security: isolated worker, parser/authorizer/resource limits, generated RLS views/triggers, short-lived grants, durable storage reservations, and explicit PostgreSQL compatibility differences.
- Billing: keep the Free, pay-as-you-go, and Pro platform catalog separate from application sales. Automated per-organization usage metering and reporting, operator-configured platform subscriptions, and project commerce with BYO Stripe or Connect direct charges are implemented.
- Deployment: separate the all-dependencies-included, loopback-only `single-host` evaluation profile from the external-provider production profile, native systemd components, and planned managed service.
- CTAs: documentation, local quickstart, GitHub source, and management portal. No cloud signup or free-tier language.

## Responsive and accessibility requirements

- Collapse two-column sections and three-column deploy cards below 900px.
- Hide decorative canvases when space is constrained; preserve the headline and primary actions.
- Mobile navigation must trap no focus and close on selection or Escape.
- All controls have visible focus states, buttons declare types, the code tabs expose `aria-selected`, and decorative SVG/canvas content is hidden from assistive technology.
- Semantic landmarks and heading order are preserved. Animation honors `prefers-reduced-motion`.

## 2026-08-03 browser verification

- Desktop 1280 × 720: verified hero hierarchy, fixed/floating navigation, install workflow, billing cards, and deployment cards against `source-desktop.png`.
- Mobile 390 × 844: verified the collapsed navigation, serif headline, readable hero copy, full-width actions, and decorative ASCII geometry in `output/playwright/landing-mobile-390x844.png`.
- Copy differs from the prototype: hosted signup and unverified performance claims became the zero-source release path, current FFDB architecture, and explicit billing status.
- The accepted warm paper, grid, serif/sans/mono hierarchy, pill actions, ASCII geometry, and restrained motion remain faithful to the source.
