---
name: ghostai-design
description: The design system rules for the GhostAI web UI — the token layer, the four core principles (minimalist, fast, every asset local, accessible by construction), and the gates that enforce them. Use when touching anything under packages/web — stylesheets, tokens, components, colours, spacing, type, icons, the theme — or when adding a UI dependency, choosing a colour, or changing how something looks.
---

# GhostAI design system

The UI is a self-hosted agent's control surface: dense, dark-first, read by people
watching a machine touch their filesystem. Every rule below exists because the
opposite shipped first and was wrong.

## Before you change anything

**Read `packages/web/src/styles/tokens.css`.** It is the vocabulary the entire UI
is written in, and its comments carry the reasoning for values you would
otherwise "fix". Two examples of what is not obvious: `--surface-0` and
`--surface-3` share a value in light and are three steps apart in dark; the
accent's lightness is chosen so it is _not_ mistakable for `--fg-1`.

**Run the gates before you believe you are done:**

```
pnpm --filter @ghostwire/web lint    # eslint + the three token gates
pnpm --filter @ghostwire/web test    # includes contrast + stylesheet assertions
pnpm check                          # typecheck, lint, full suite
```

## The four principles

### 1. Minimalist — fewer variables, fewer variants, one way to say a thing

Complexity is the failure mode this system is guarding against. More tokens is
worse. More size variants is worse.

- **Do not add a token without deleting one, unless the need is structural.** A
  special case that gets its own seed is a special case wearing a scale's
  clothes. A role that needs to be louder than its siblings is not a reason for a
  `--seed-<role>-c`.
- **A size variant with one caller is not a variant.** `btn--lg` and
  `--size-control-lg` were both removed for exactly this: their only caller was
  the style guide demonstrating that they existed.
- **Say selection one way.** A raised surface (`--surface-3`) plus the
  full-strength text tier — in the sidebar, in menus, in tabs. A tick is
  _confirmation_ after that, never the only signal, and never in the accent
  colour competing with the row's own icon.
- **Scales are ramps, not number lines.** Steps must be far enough apart that a
  human can pick between them for a reason. Spacing has nine steps and doubles
  roughly every one; two radii and a pill; two control heights; two icon sizes.
- **Delete rather than deprecate.** Dead exports read as live to the next person.

### 2. Fast

- Nothing heavy in the entry chunk. Syntax grammars and highlighters load on
  first use behind a dynamic `import()` with an **explicit table** — never a
  computed import path, which makes the bundler emit every branch and defeats the
  splitting entirely.
- No layout thrash on first paint. The theme is stamped by a blocking inline
  script in `index.html` before anything renders; a deferred correction one frame
  later is the flash it exists to prevent.
- Prefer CSS to JavaScript for state CSS can express. A checked radio styling its
  own sibling beats a class recomputed in React.
- **Derive geometry with `calc` from one chosen number.** Numbers that must agree
  eventually will not: the switch had five typed independently and two were
  wrong, so its thumb was squeezed against a track it also overshot.

### 3. Every asset local — no CDNs, ever

This is self-hosted and privacy-first, and some installs are air-gapped.

- Fonts come from npm (`@fontsource-variable/*`), emitted into `dist/assets` by
  the bundler. Never a font CDN, never a Google Fonts `<link>`.
- No `preconnect`, no `dns-prefetch`, no analytics beacon, no remote image, no
  script tag pointing at another host.
- A first paint that reaches a third party leaks every user's IP to it. On a
  machine with no route out it is a page rendering in Times New Roman while a
  fetch times out.
- `packages/web/test/self-contained.test.ts` scans the shipped sources for any
  external origin. It is a test rather than a note because the rule is far easier
  to hold than to restore.

### 4. Accessible by construction, not by audit

- Every text-on-surface pairing meets WCAG AA in **both** themes.
  `src/tokens/contrast.test.ts` resolves the real stylesheet and measures it, so
  a seed edit that darkens text past the line fails the suite. When it does,
  **compute the corrected value** — do not nudge until it passes.
- **Native elements first.** A hidden `<input type="radio">` behind a drawn face
  buys arrow keys, the roving tab stop and the right announcement for free; a
  `<div role="radio">` buys a reimplementation of all three.
- The focus ring is defined once in `base.css` and never removed. Adding a ring
  somewhere more visible is fine; `outline: none` is not, and `a11y.test.tsx`
  sweeps for it.
- **Never `opacity` for a disabled state.** Fading text and its background by the
  same factor does not preserve their contrast. Use a measured pairing.
- Status is ambient. A transient state must not be the loudest thing on screen —
  the connection indicator is a dot and a word, not a filled amber pill.

## The token layer

`packages/web/src/styles/tokens.css` is the **only** file allowed to contain a
raw colour or a `px` literal. `src/tokens/gates.ts` enforces three rules across
CSS, TSX and `index.html`:

1. **No `px` outside `tokens.css`.** Density comes from the type scale and the
   root font size is never overridden, so a `px` literal is a piece of UI that
   opted out of the user's browser setting. `--hairline` is the one exception.
   _This gate reads comments too_ — writing "16px" in prose fails it.
2. **No raw colour outside `tokens.css`.** A hex in a component is a colour that
   cannot follow the theme and was never measured.
3. **`--accent` fills; `--accent-fg` is for text, icons and strokes.** They are
   identical in dark and diverge in light, so this is invisible in the theme most
   work happens in — which is exactly why it is a lint rule.

Beyond the gates:

- **Every colour derives from a per-theme seed block.** A theme is two dozen
  numbers and the tokens are formulas over them. Adding a role adds four seeds,
  not two — a fill, its text variant, and a chroma for each, because sRGB holds
  far less chroma at low lightness.
- **Every length is a rem.** The root font size is never set, in any unit;
  `html { font-size: 62.5% }` is the same mistake wearing a disguise.
- **Elevation is a surface and a stroke, not a shadow.** There is no shadow
  scale. A drop shadow needed per-theme tuning to be visible at all — black at
  18% over a page at L 0.148 is a 3-of-255 difference — and a token whose value
  must quadruple between themes to mean the same thing was doing two jobs.
- **The style guide at `/tokens` builds token names from data** (`var(--${role}-soft)`).
  Template strings are invisible to any search for a token's callers, so a token
  can look unused while that page renders it. `routes/tokens.test.tsx` asserts
  every `var()` in its output resolves to a declared token — keep it passing.

## Type and the mark

- **Sans for prose, mono for identifiers.** Inter is the UI font; JetBrains Mono
  carries the things read character by character — the wordmark, the resolved
  model, badges, uppercase micro-labels, code.
- **Never track a monospace tighter.** Every glyph sits on a fixed advance, so
  negative letter-spacing does not tighten the word, it eats sidebearings drawn
  to be equal. A hair of positive tracking is what a mono wordmark wants.
- **Icons come from `lucide-react`. All of them, including the brand mark.** The
  mark is lucide's `Skull`, imported like any other icon.

  This is the most expensive lesson in the file. The mark was hand-drawn for four
  revisions and redrawn three times — organic, then hard-edged geometric, then
  rounded geometric — while it kept "not fitting". None of those were the
  problem. It was a _filled silhouette_ in a UI where every other glyph is a
  lucide stroke icon: `fill: none`, 2-unit stroke, round joins on a 24 grid. A
  solid shape beside line icons is not a different style, it is a different
  weight class, and no amount of adjusting cheekbones and corner radii was ever
  going to close that gap.

  Two rules fall out of it. **Match the neighbour that matters** — an icon's
  neighbours are other icons, not the layout's corner radii, which is the wrong
  comparison that produced the geometric drafts. And **check the set before
  redrawing the piece**: if something looks wrong beside its siblings, render it
  beside its actual siblings at the actual size before touching its geometry.

- **The favicon is the brand hue at a lightness the accent does not use.** A tab
  strip is the one surface the app does not control: white in one OS theme and
  near-black in the other, so the mark is pulled to mid-luminance to hold against
  both. It is the package's one duplicated drawing — a tab has no React to get
  lucide's stroke defaults from, so the shapes and attributes are written out —
  and `favicon.test.ts` reads them back off a rendered `<Skull />` rather than
  pasting them into an assertion, so a lucide upgrade that redraws the icon fails
  there instead of leaving the tab on the old one forever.

## When a rule is worth holding

Add the test, not the comment. The question is never whether to note something —
it is which file asserts it. Every rule above is enforced somewhere, and that is
why they survived the redesigns that broke them.
