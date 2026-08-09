# Multi-instance portal design QA

Verified on August 3, 2026 against the packaged Docker gateway at
`http://127.0.0.1:5173/app/`.

## Capture method

- Desktop: in-app browser screenshot at its native 1280 × 720 viewport.
- Phone: responsive browser viewport at 390 × 844, followed by DOM and
  full-page screenshot verification.
- First-run and project flows used the live Axum API, PostgreSQL control plane,
  project SQLite worker, object storage, and gateway rather than mocked data.

## References and renders

- Desktop concept: `apps/portal/design/multi-instance-shell-concept.png`
- Mobile concept: `apps/portal/design/multi-instance-mobile-concept.png`
- Onboarding concept: `apps/portal/design/first-run-onboarding-concept.png`
- Desktop render: `.design/portal-multi-instance/renders/project-overview-desktop.png`
- Phone render: `.design/portal-multi-instance/renders/project-overview-mobile.png`
- Fresh-owner render: `.design/portal-multi-instance/renders/fresh-owner-desktop.png`

## Comparison

1. The navy scope rail, white workspace, fine rules, blue primary actions, and
   green health treatment match the accepted Atlas visual system.
2. Instance, organization, and project are explicit, independent selectors;
   the same three scopes become stacked rows on phone layouts.
3. Workspace, Build, Operate, Sell, and Administration navigation groups match
   the approved information architecture and collapse into a mobile drawer.
4. The live overview preserves the concept's service-health rail, usage,
   activity, quick-action, and worker-status hierarchy while showing real API
   values.
5. The four-stage Owner → Instance type → Payments → First workspace sequence
   and the locked-before-completion behavior match the onboarding concept.

## Intentional differences

- At 1280 × 720, the live overview keeps the sync chart above recent activity;
  the concept placed activity first. The existing product's sync-operability
  emphasis was preserved while the scope/navigation redesign was applied.
- At 390 px, service cards use a contained horizontal snap rail instead of
  compressing five cards into unreadable columns. The document remains exactly
  viewport-width with no page-level horizontal overflow.
- The owner bootstrap is a dedicated secure screen before the larger instance
  setup shell because the instance does not yet have an authenticated owner.

## Behavioral acceptance

- Fresh owner creation, private-mode setup, organization creation, and project
  creation completed through the UI.
- The API returned `409 instance.setup_required` for organization creation
  between owner bootstrap and setup completion.
- Two projects were created and switched Atlas → Beacon → Atlas. Each restored
  its own scoped key and loaded a healthy overview.
- The final local volumes were deleted and the handoff instance was returned to
  the untouched owner-creation screen.
