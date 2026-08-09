# Landing style parity QA

## Evidence

- Accepted desktop reference: `source-desktop.png` at 1280 × 720.
- Accepted mobile reference: `source-mobile.png` at 390 × 844.
- Final desktop render: `render-parity-desktop.png` at 1280 × 720.
- Final mobile render: `render-parity-mobile.png` at 390 × 844.
- Rendered in the in-app browser from the Vite application and inspected directly with `view_image`.

## Comparison ledger

| Area | Reference evidence | Final render evidence | Result |
| --- | --- | --- | --- |
| Header | Unframed first-viewport navigation, compact wordmark, dark pill CTA | Same proportions and alignment; scrolled state becomes the original floating pill shell | Matched |
| Typography | Large geometric sans headline, compact mono label, neutral sans supporting copy | Restored Avenir/Helvetica display and body stack; removed the divergent editorial serif treatment | Matched |
| Background | True warm-white canvas, quiet grid, monochrome ASCII sphere | Same canvas temperature, grid spacing, fade, and ASCII sphere treatment | Matched |
| Controls | Black and outline pill buttons with restrained hover motion | Reused the same pill geometry, border weight, spacing, and interaction states | Matched |
| Responsive composition | 24 px mobile gutter, stacked compact CTAs, sphere centered behind the lower hero, marquee near 722 px | Mobile gutter is 24 px, page width is exactly 390 px with no overflow, CTAs stack at content width, sphere and marquee align to the reference rhythm | Matched |
| Navigation behavior | Full-screen mobile menu and compressed scrolled desktop header | Both states are implemented; menu icon becomes a true close icon and body scroll locks while open | Matched |

## Copy diff and intentional deviations

The legacy reference contains prototype labels and claims such as “The platform to create,” “Pricing,” and “Start creating.” The final render intentionally keeps the released product’s current headings, installation CTA, architecture claims, billing navigation, and live routes. The longer current hero description pushes the mobile CTA group lower than the prototype copy; typography, gutter, button dimensions, sphere placement, and marquee position still follow the accepted component system.

No material visual mismatch remains.
