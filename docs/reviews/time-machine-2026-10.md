# The time machine: a UX review (2026-10-22, task TM-UX)

> **In short.** The time machine already does the hard parts well: two modes
> that say in words what the graph shows, tables that never move, colors that
> read as a map at overview zoom, a keyboard path through every control. The
> review found contrast failures where things step back (text faded with
> opacity), information only the eye could get (how much each version
> changed), a legend whose first key could not be seen nor matched its bar,
> range handles nobody is told about, a mode switch too small for a thumb,
> and a phone layout that hides the mode and the legend. Seven are fixed
> here, each with a test; the phone layout, a first-run hint, the mode names
> and long histories are proposals (section 3).

What it is: docs/MVP.md "The time machine"; the code is `web/src/features/time-machine`.

## How it was reviewed

- **Screens.** The dev route `/app/dev/time-machine` on its two fixtures (a shop,
  8 versions; 220 tables over 120 versions), headless Chromium at 390, 1024,
  1440 and 1728 px, light and dark: the default view, the cursor moved back
  (both modes), a table selected, a narrowed range, the timeline sheet and the
  filters popover on a phone, a range handle hovered. The real page
  (`/app/<org>/<db>/time-machine`) wraps the same component in the database's
  header, with the Compare button and the removed versions' eye: those were
  read in the code, not run (the API was not built on this 8 GB machine).
- **Contrast** measured on the rendered page: every text run's color with its
  own and its ancestors' opacity composited over what is behind it, both
  themes.
- **better-design**: the comprehension check (the screen's copy), the UI and UX
  principles on opacity, contrast, legends and sliders, the spacing inspection
  (section 4).
- **Performance**: `e2e/perf/time-machine.perf.spec.ts` headless on a
  production build, report only (section 5).
- **Not done**: no session with a real screen reader; what is said about
  announcements comes from the ARIA.

## 1. What works

- **The modes say what they show.** The chosen mode is inverted, and the line
  under it says it in words: "All 8 versions superposed, v4 highlighted", "The
  schema at v4". A newcomer who does not know the word *superposed* still
  learns what the graph is.
- **Nothing jumps.** Tables keep their place for the whole history; moving the
  cursor or the range only fades, strikes through or animates in what changes.
  Scrubbing reads as the schema changing, not as the layout changing.
- **Color is never alone.** Every age color comes with a word: `+v4` / `−v7`
  badges on tables, strike-through for what is gone, `~` for what changed, the
  legend spelling out which versions each color covers (`v2–21`, `v22–41`…).
  At overview zoom, the 220-table fixture is a map of where the schema grew.
- **The keyboard reaches everything, in reading order**: range start, range
  end, cursor (real sliders: arrows, Page Up/Down, Home/End), play, previous,
  next, the modes, the filters; each with a visible ring. `[`, `]`, K and M
  are in the tooltips and in the graph's "Legend and shortcuts".
- **The selected table's history** lists every version that touched it or its
  parts, each a link to that version's diff.
- **Edges of the domain are handled**: no version and one version explain what
  the time machine needs, with the action that fills it; a history that fails
  to load says so, with Retry; the state is in the address (shareable).
- **The phone layout holds**: no horizontal scroll at 390 px, a sheet that
  opened from the keyboard puts focus on the cursor, a selected table's history
  inside the sheet.

## 2. Problems, by impact, and what was done

| # | Problem | Impact | Status |
|---|---|---|---|
| 1 | Faded text under 4.5:1 | WCAG 1.4.3 failure | Fixed |
| 2 | How much each version changed: eyes only | WCAG 1.1.1 / 1.3.1 | Fixed |
| 3 | The legend's first key invisible, unlike its bar | Comprehension, WCAG 1.4.11 | Fixed |
| 4 | Range handles never introduced | Discoverability | Fixed |
| 5 | Each cursor step read twice | Screen reader noise | Fixed |
| 6 | A tooltip opening over the filters popover | Visual glitch, < 1280 px | Fixed |
| 7 | The mode switch 28 px tall on a phone | Touch target | Fixed |
| 8 | On a phone, the mode and the legend are hidden | Comprehension at 390 px | Proposal (3.1) |
| 9 | No first-run explanation | Onboarding | Proposal (3.2) |
| 10 | "Superposed" | Vocabulary | Proposal (3.3) |
| 11 | Very long histories on the track | Precision past ~300 versions | Proposal (3.4) |
| 12 | Selection dims other tables with opacity | Contrast, shared viewer | Proposal (3.5) |
| 13 | The kit's toggle has no phone size | Touch targets, app-wide | Proposal (3.6) |

### 1. Text faded with opacity fell under 4.5:1 (fixed)

What steps back was faded with `opacity`, which takes the text with it:

- the legend's runs after the cursor (`opacity-50` on the whole entry): the
  version numbers at **3.35:1** in light, 4.40:1 in dark;
- the history panel's events outside the range (`opacity-60` on the row): the
  author and date at **2.88:1** in light, 3.34:1 in dark.

**Fix**: the words step back in ink, not opacity. In the legend only the
swatch fades; the version numbers go from `text-foreground` to
`text-muted-foreground` (7.6:1 light, 7:1 dark). In the history panel, outside the range, the
event goes to `text-muted-foreground` and its author and date to
`text-faint-foreground` (4.8:1 light, 4.7:1 dark); the version chip's ring
lightens. Every text run of the timeline and the panel is at 4.5:1 or more in
both themes now.
**Test**: "what steps back (versions after the cursor, events outside the
range) keeps 4.5:1" (e2e/dev, both themes, the composited color).

### 2. How much each version changed reached only the eye (fixed)

The bars' heights say how much each version changed; they are `aria-hidden`,
and the sliders' values said which version, never how much. The hover card
said it, but as "1 changes".
**Fix**: each slider's `aria-valuetext` ends with it ("v8, Invoices, push,
Ada, Sep 8, 7 changes"), and the card words it the same way (`changesText`:
"no change", "1 change", "12 changes").
**Tests**: "the sliders say how much each version changed…" (e2e/dev),
`changesText` in `frame.test.ts`.

### 3. The legend's first key could not be seen, nor matched its bar (fixed)

The timeline is meant to be the legend (MVP.md). But the range start's bar was
the darkest mark on the track (a `muted-foreground` fill), while its key, "In
v1", was a white square ringed in `border-strong`: 1.5:1 on the card, close to
invisible. "Not yet in v8" had the same problem (a `border` ring, 1.3:1). A newcomer saw a dark
bar the legend did not explain.
**Fix**: the start's bar and its key are drawn alike, uncolored and outlined
in `faint-foreground` (4.8:1 light, 4.7:1 dark); "Not yet in" keeps a lighter ring, still
visible next to it.
**Test**: "the range start's bar looks like its key in the legend" (same
background and ring, the ring at 3:1 or more).

### 4. The range handles were never introduced (fixed)

The two handles are small outlines at the ends of the track; hovering one
showed the version under it, as anywhere on the track. Nothing said it was a
handle, nor that it drags, until someone dragged it by chance. The range is
what the colors, the header and Compare are about, so missing it means missing
half of the view.
**Fix**: over a handle, the card names it and says what it does: "Range start ·
drag to change the range", then the version it is on.
**Test**: "hovering a range handle says which it is and that it drags".

### 5. Each step of the cursor was read twice (fixed)

The cursor slider's value and the readout beside the play button (a polite
live region) both said the version at each arrow press.
**Fix**: while a slider of the track has focus, the readout keeps quiet; it
speaks again for the buttons and `[`/`]`, which have no value of their own to
read. Not verified with a screen reader (see "How it was reviewed").
**Test**: the readout's `aria-live` is `off` on a slider and `polite` once
focus moves to Play.

### 6. A tooltip opened over the filters popover (fixed)

Under 1280 px the filters live in a popover. Opening it focuses "Only
changes", whose tooltip then opened at once and covered the popover's top.
**Fix**: no tooltip in the popover; a line under the filters says what they do,
which is also what surprises a newcomer filtering by name: "What the filters
leave out fades back; no table moves." (MVP.md: filters fade, never hide.)
**Test**: "the filters' popover says what they do, no tooltip over it" (1024 px).

### 7. The mode switch was 28 px tall on a phone (fixed)

The spacing inspection (section 4) measured the sheet's "Superposed" and "At
cursor" at 28 px: the kit's buttons step up to 36 px on a phone, its toggle
group does not. Above WCAG 2.2's 24 px, but the smallest thing a thumb has to
hit on the screen, and the only way to switch modes there (no M key).
**Fix**: in the time machine, under `md`, the mode items are 36 px tall like
the bar's buttons. The kit itself is left alone (proposal 3.6).
**Test**: the phone test checks both items are 36 px tall or more.

## 3. Proposals (not built)

### 3.1 On a phone, keep the timeline in view (serious at 390 px)

Collapsed, the phone's bar shows play, step, the cursor's version, filters and
a bare chevron. The mode, the range and the legend are behind the chevron, so
the colors and the faded tables arrive unexplained, and M (the mode's key) does
not exist on a phone.

Proposed: a strip of the track, about 28 px, always visible above the transport
on a phone: the bars in their colors and the playhead, no labels, no handles.
Tapping or dragging it scrubs the cursor; a tap on its edge (or the chevron,
then labelled "Timeline") opens the sheet for the range and the legend. The
readout gets a second line in `text-xs`: "Superposed" or "At v5". Picture
it as the 390 px bar of today with the eight colored bars of the sheet
squeezed into a thin band above it; the graph loses 28 px.

### 3.2 A first-run hint (moderate)

The screen relies on a newcomer finding the handles and trying both modes.
The comprehension check counted 110 words and 8 actions (it estimates 58 s to
explain); for a dense tool the count matters less than the missing *how*.
Proposed: on a person's first visit, one dismissible line under the track,
remembered per person: "Drag the handles to choose versions, then play: each
table takes the color of the version that added it." No coach marks.

### 3.3 The words "Superposed" and "At cursor" (moderate)

"Superposed" is MVP.md's word; it is rare English, and "cursor" makes people
think of the mouse. The caption under the modes explains both, so this is
polish: "All versions" / "One version" would say the same in common words.
Decide in MVP.md first (it names the modes), then change the toggle, the
caption, the tooltips and the specs together.

### 3.4 Long histories on the track (moderate past ~300 versions)

At the cap of 1000 versions, the track gives each version about 1.3 px at
1440 px: the bars overlap, hovering one version is pixel hunting, the labels
come every ~35 versions. The keys still work (Page Up/Down move a tenth).
Proposed: when versions get narrower than ~4 px, the track becomes two rows: an
overview strip of the whole history with the range as a window on it, and the
detail track below showing only the range, at full width. Dates as ticks
(weeks or months) help as much as numbers there.

### 3.5 Dimming by opacity in the schema viewer (minor, out of scope)

Selecting a table dims the others to 35 % opacity: their words fall to about
2.2:1. The nullable `?` on an era-tinted row is at 4.0–4.25:1. Both belong to
the schema viewer, shared with the other pages. Proposed: dim with tokens (a
faded border and `faint-foreground` text), as the time machine's own faded
states already do.

### 3.6 A phone size for the kit's toggle group (minor, app-wide)

The kit's `Button` is 36 px on a phone and 32 px from `sm` up; its
`ToggleGroup` is 32 px (items 28 px) everywhere. The time machine now steps
its own up on a phone (fix 7); the kit could do it once for every page (the
diff's views, the migration's tool picker), as `Button` does. The bar's
36 px buttons themselves are under the 44 px of the platforms' guidance:
also a kit-wide decision, not one for this view.

## 4. Spacing

`inspect-spacing` on the timeline section (the track, the legend, the bar),
light, at 390 (sheet open), 1440 and 1728: completed, 16 serious and 27
moderate findings, no critical one. Read one by one:

- **Touch targets** (serious for the sliders). The handles (10 × 20 px) and
  the cursor's chip are false positives: the track picks the handle by
  distance (a finger gets 44 px around it, a pointer 24 px) and a press
  anywhere else on its 88 px moves the cursor. Real: on a phone, the mode
  items at 28 px (fixed, 7) and the 36 px buttons (3.6). On desktop, 26 to
  32 px controls with a mouse: above the 24 px minimum.
- **Uneven gaps in the bar** (0, 8, 8, 8 px): the 0 is the screen-reader-only
  live region, a 1 × 1 px absolute element. False positive.
- **Clipped content** in the author's avatar: its screen-reader-only name,
  clipped on purpose. False positive.
- **The name filter's padding** (0 left, 10 right): the kit's `Input` with a
  prefix, which brings its own 10 px. False positive.
- **Off-scale 2, 6 and 10 px** (moderate): Tailwind half steps (`gap-1.5`,
  `pb-1.5`, `gap-2.5`, `px-1.5`) from the timeline and the kit, measured
  against a scale without half steps. The app uses them everywhere; nothing
  to change for this view alone.

## 5. Performance

The perf spec, headless, on a production build (report only: GPU frame times
mean little headless, and the spec's budgets are for headed runs):

| Step | Median frame | p95 |
|---|---|---|
| First render (220 tables, first layout) | 660 ms in all | |
| Scrub forward, overview | 83 ms | 133 ms |
| Scrub forward, zoomed in | 100 ms | 133 ms |
| Play for 6 s | 67 ms | 133 ms |
| Switch modes and back | 83 ms | 133 ms |

Frames come in multiples of 16.7 ms, the mark of software rendering on a
loaded 8 GB machine; they are not comparable with the spec's headed budget
(p95 < 20 ms). Nothing in this review touches the graph's rendering (the
legend and the track rerender as before, on the same state). Run `just perf`
headed on a quiet machine before drawing conclusions; with 1000 versions the
track redraws a bar per version on every step, which section 3.4 would also
reduce.

## 6. Checks

- `npm --prefix web run typecheck`, `lint`, `format:check`, `test` (vitest):
  green.
- `npx playwright test -c playwright.dev.config.ts time-machine --workers=1`:
  10 passed. The five new tests fail on the components before this change;
  the phone test's new check would on the 28 px items the spacing
  inspection measured.
- `web/e2e/time-machine.spec.ts` (needs the stack) was not run; its
  assertions on the sliders match on a prefix (`/^v3, /`), which the added
  "N changes" leaves alone.
