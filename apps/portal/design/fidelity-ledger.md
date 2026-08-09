# Atlas overview fidelity ledger

Reference: `atlas-overview-concept.png` at 1536 × 1024. Verified implementation: `atlas-overview-render.png` at the same viewport and density.

| Surface | Reference behavior | Implemented result | Evidence and disposition |
| --- | --- | --- | --- |
| Application shell | 228 px ink sidebar, 60 px project bar, cool near-white canvas | Dimensions, fixed positioning, rules, and responsive conversion match | Full-view comparison; passed |
| Identity and navigation | Organization/project context and eleven selected/hoverable project routes | Live organization and project names; all routes are semantic buttons with selected and focus states | Shell comparison and browser navigation; passed |
| Page hierarchy | 42 px Atlas title with one-line helper above the dashboard | Title scale, weight, spacing, and copy align | Shell comparison; passed |
| Health and usage | Five service cells plus compact four-row usage rail | Live readiness, schema, policy, storage, backup, and request data fill the same geometry | Shell comparison; dynamic values accepted |
| Sync activity | Three toggleable colored series, time legend, grid, and labels | Series are derived from recent project activity; each legend changes the plotted data and pressed state | Detail comparison and interaction check; passed |
| Recent activity | Five dense, actionable rows with actor, resource, result, and pagination | Live audit entries populate the table; rows are keyboard-focusable and pagination changes page state | Detail comparison and browser interaction; passed |
| Quick actions | Four prominent routes with Run SQL emphasized | Every action routes to its working portal surface and emits visible command status | Browser interaction; passed |
| Worker status | Compact status, numeric details, two meters, metrics affordance | Available local route, request activity, and RLS coverage are reported without fabricated host values | Detail comparison; accepted product-aware adaptation |
| Responsive layout | Right rail reflows below 1200 px; horizontal navigation and one column below 760 px | 1024 px and 740 px states match; no document-level horizontal overflow | Browser measurements and responsive captures; passed |
| Visual tokens | Cool canvas, ink sidebar, white square panels, cobalt/green/orange accents, no decorative shadows | Tokens, radii, borders, controls, and status colors align | Full and focused comparisons; passed |
| Platform billing | Prototype had no organization-entitlement surface | Added a live Free/PAYG/Pro summary, enforced project limit, allowances, provider state, and role-gated Checkout/Portal actions in the accepted dark zinc/green system | `output/playwright/portal-billing-desktop-1280x720.png` and `portal-billing-mobile-390x844.png`; passed with synthetic read-only API fixtures |
| Project commerce | Prototype conflated application sales with platform billing | Project-owned BYO Stripe and Connect direct-charge accounts now back products, immutable prices, Checkout, orders, subscriptions, entitlements, refunds, and paid fulfillment; this remains separate from organization usage billing | Existing payment-layout captures establish the visual baseline; live commerce-state browser acceptance is tracked in the release matrix |
| Sign-in refresh | Centered dark account card | Preserved the compact card, muted chrome, green primary action, and responsive spacing while changing copy to the platform-owner flow that exists | `output/playwright/portal-signin-mobile-390x844.png`; faithful product-aware adaptation |

The source and implementation contain no raster media inside the app. Icons and charts are rendered as scalable interface primitives so they remain crisp and interactive. Desktop and 390 × 844 browser QA reported zero console errors; authenticated billing/payment captures used synthetic network fixtures and did not modify the preserved local FFDB data.
