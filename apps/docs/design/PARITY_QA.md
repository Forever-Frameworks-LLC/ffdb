# Documentation shell style parity QA

## Evidence

- Accepted desktop reference: `source-desktop.png` at 1280 × 720.
- Accepted mobile reference: `source-mobile.png` at 390 × 844.
- Final desktop render: `render-parity-desktop.png` at 1280 × 720.
- Final mobile render: `render-parity-mobile.png` at 390 × 844.
- Rendered in the in-app browser from the Vite application and inspected directly with `view_image`.

## Comparison ledger

| Area | Reference evidence | Final render evidence | Result |
| --- | --- | --- | --- |
| Brand | Teal FFDB database mark and compact FFDB wordmark | Restored the original mark geometry, gradient, glow, and 24 px desktop lockup | Matched |
| Desktop shell | 320 px fixed sidebar and 56 px top bar | Sidebar is exactly 320 px; header begins at x=320 and remains 56 px high | Matched |
| Content geometry | Article begins at x=400 with compact documentation typography | Article begins at x=400; title is 26 px desktop / 24 px mobile and body density matches the reference | Matched |
| Search and controls | 416 px search field, restrained top-level links, green pill action | Search is 416 px at 1280 px; controls use the original quiet border and emerald treatment | Matched |
| Themes | Zinc dark mode, white light mode, subtle green hero field | Both themes were exercised; control backgrounds, code colors, callouts, and the hero field remain legible in each | Matched |
| Mobile shell | 56 px bar with menu, FFDB wordmark, search, and theme controls | Same order and spacing; 390 px viewport has no horizontal overflow | Matched |
| Navigation interactions | Expandable groups, current-page rail, mobile drawer, keyboard search | Group disclosure, current-page marker, scrollable drawer, Escape handling, search dialog, and body scroll locking all work | Matched |

## Copy diff and intentional deviations

The current documentation has a broader installation, billing, operations, and generated-reference information architecture than the prototype. Those released routes and their live-product wording remain intact. The original shell and component language are applied to that expanded content rather than reducing the navigation or replacing end-user documentation with prototype copy.

No material visual mismatch remains.
