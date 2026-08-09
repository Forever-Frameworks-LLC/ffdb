# FFDB Design QA

## Product direction

- The landing site uses a restrained neutral palette, strong editorial hierarchy, and green only for primary actions and positive state.
- The documentation is content-first: a compact introduction, clear entry paths, persistent navigation, readable code, and an optional light/dark theme.
- The portal is an operational workspace. Scope selectors and the left navigation establish location; page-local controls only switch subordinate views or tasks.
- Dense tools such as the SQL editor and database explorer use the available viewport instead of nesting the work surface inside decorative cards.

## Completed audit

- Landing: desktop, tablet, and mobile navigation; CTA hierarchy; release-oriented copy; keyboard focus; reduced motion; overflow.
- Documentation: all 31 routes; headings and table of contents; code highlighting; install paths; desktop and mobile layouts; light and dark themes.
- Portal: signed-out, bootstrap, workspace, database, migrations, policies, auth, storage, sync, email, activity, backups, usage, commerce, settings, instance administration, and billing states.
- Shared patterns: scope dropdown hit areas, page headings, tabs, tables, forms, empty/error/loading states, responsive behavior, theme persistence, and prepaint behavior.

## Verification

- Portal tests: 101 passed.
- Documentation tests: 25 passed.
- Landing tests: 10 passed.
- Portal, documentation, and landing production builds: passed.
- Documentation route render sweep: 31 of 31 passed with no horizontal overflow and highlighted code on every code-bearing route.
- Live Docker gateway: landing, `/docs/`, `/app/`, and `/readyz` passed; portal light mode persisted through reload.
- CSP: CodeMirror uses the named `ffdb-codemirror` style nonce; the gateway does not enable unrestricted inline script execution.

## Acceptance reference

Follow `docs/testing/manual-acceptance.md` for the complete authenticated workflow and breakpoint matrix.

final result: passed
