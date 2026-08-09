# Original portal parity fidelity ledger

## Accepted reference

- `/Users/seancotter/Desktop/dev/learning/ffdb/private-code/app/src/index.css`
- `/Users/seancotter/Desktop/dev/learning/ffdb/private-code/app/src/components/ui/`
- `source-signin-desktop.png`

## Render evidence

- `original-parity-owner.png`
- `original-parity-overview.png`
- `original-parity-mobile.png`

## Closed differences

| Difference | Resolution |
| --- | --- |
| Cool blue, edge-to-edge application shell | Replaced with the original neutral OKLCH system palette and inset, rounded application shell. |
| Generic portal branding | Reintroduced the original FFDB mark and lowercase lockup across onboarding, authentication, desktop, and mobile. |
| Large controls and low information density | Matched the original compact button, field, card, table, and navigation geometry. |
| Mobile navigation could only be dismissed indirectly | Added an explicit accessible close action and verified the drawer at 390 x 844. |
| Fresh portal keys could not open commerce or rotate signing keys | Included `commerce_manage` and `keys_rotate` in owner-created and generated project keys and covered the request in tests. |

## Intentional adaptation

The current Rust portal's multi-instance, organization, project, billing, and commerce information architecture is preserved. It is rendered through the original application's visual language instead of removing current functionality to reproduce the prototype's smaller feature set.

Above-the-fold product content and task order were preserved; this pass changes presentation and fixes the fresh-key capability gap.
