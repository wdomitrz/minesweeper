# minesweeper

Minesweeper: three classic boards, flags, a clock and a first click that cannot
lose. All of it — the mine placement, the safe first click, the flood reveal, the
flag and mine counts, the clock and the saved game — is Rust. `cargo build`
generates the **static PWA** into `./dist`: a front-end-only site any file host
can serve, with no server and no runtime dependency on a binary.

AGPL-3.0-only. See `LICENSE`.

This is a fork of the public
[`wdomitrz/minesweeper`](https://github.com/wdomitrz/minesweeper), whose whole
content was a licence — `LICENSE` and one commit. There is no original
implementation to preserve, so this is the app rather than a rewrite of one, on
the pattern of `bot-git-ai/sudoku` and the Rust PWA template.

## Build and run

Two builds, because there are two targets. Nothing generated is committed.

```
# 1. the app: compile the crate to wasm and run the bindings generator
rustup target add wasm32-unknown-unknown
cargo build --locked --lib --target wasm32-unknown-unknown --release
wasm-bindgen --target web --no-typescript --out-dir dist --out-name app \
  target/wasm32-unknown-unknown/release/minesweeper.wasm

# 2. the rest of the site
touch build.rs
cargo build --release --locked
```

Step 1 writes `dist/app.js` and `dist/app_bg.wasm`; step 2 adds the six files
`build.rs` owns — `index.html`, `service-worker.js` with its version substituted,
`icon.svg`, the two rasterized icons, and the manifest — and derives the service
worker's cache version.

**The order matters, twice over.** `build.rs` derives the cache version from the
bytes of every other file in `dist/`, the wasm included, so running it first
would pin the version to whatever the previous build left behind. And the `touch
build.rs` is not redundant: `build.rs` writes into the source tree rather than
`OUT_DIR`, so cargo cannot see that anything changed and will not re-run it for a
second, otherwise identical invocation — which leaves a half-built site holding
the two wasm artefacts and none of the shell files.

There is **no run step and no server**: the build is the whole story. `INSTRUCTIONS.md`
is the template's contract, kept as-is; this file records the decisions taken
on top of it.

`wasm-bindgen` is pinned to `=0.2.128` in `Cargo.toml` and the CLI must match
exactly — a mismatched generator produces bindings the runtime will not load and
the page fails with "Minesweeper could not start".

## Where the rules live

- `src/minefield.rs` — the board: how big, where the mines are, how many touch
  each square. Mines are placed on the **first reveal**, with that square and its
  eight neighbours left clear, which is what makes a first click safe; the
  placement is seeded, so the same seed and first square rebuild the same board
  and a saved game is a handful of numbers rather than 480 mine positions.
- `src/game.rs` — one sitting: what is covered, flagged and open, the clock, how
  it ended, and **every word the page says** about any of it. Class tokens, the
  spoken name of a square and the status sentence are all built here, so the
  board, the counter and the status line cannot disagree.
- `src/ui.rs` — the DOM, and nothing else. Compiled only for
  `wasm32-unknown-unknown`, so `cargo test` never needs a browser.

Neither domain module has `web-sys`, `js-sys` or `wasm-bindgen`, which is why the
rule set is tested natively.

## Decisions worth keeping

- **Two gestures, and the long one is the other action.** A tap does what the
  mode says; a long press does the opposite, so neither mode needs the Flag
  button. The gesture is `pointerdown` plus a 450ms timer, cancelled by
  `pointerup`, `pointercancel` and 10px of drift — a press that becomes a scroll
  must not flag the square the finger left. The browser then sends more events
  for the same gesture (a `click` on release, and on Android a `contextmenu` at
  ~500ms), so both are gated on a 700ms suppression window measured from when the
  long press fired. A flag consumed by one would let the other through and do
  both actions on one square, which on a covered square is a lost game.
- **Flag mode is a button as well.** A checkbox would toggle without a listener
  but cannot be *disabled* when the game is over, and flagging a finished board
  is meaningless. A button carries `aria-pressed` for the same information.
- **A click on an open number chords.** Whether a click opens or chords is a
  rule, so `Game::is_chordable`/`Game::chord` decide it and `ui.rs` only calls
  `Game::open`; the page never decides a rule from class names it just wrote. A
  chord acts on the *player's* claim, not the board's truth, which is why the
  mine check runs before anything opens and ends the game — and why a chord that
  loses does not leave a half-opened ring behind.
- **One tab stop, arrows to move.** 480 buttons in the tab order is not
  navigation. `roving tabindex`, with the arrow keys bounded by the board rather
  than wrapping around it.
- **The clock is a wall clock, and it carries its remainder.** An interval fires
  0.999s apart as often as 1.001s; dropping the fraction, or comparing it to one
  second, stops the clock dead. That bug shipped into the first build and was
  caught by playing the game in a browser — the unit tests pass a whole number of
  seconds, so no unit test could have seen it. Time is only banked while the game
  is being played.
- **Four boards, the last one absurd.** Insane is 99x99 with 2000 mines. It is
  drawn dense — a 1px gap, no per-square lighting, 14-22px squares — because
  9801 bevels is a texture rather than a set of buttons, and the flag is the
  level's own property rather than a measurement of the screen, so the same level
  looks the same everywhere. Its board opens inside its own scroll box on both
  axes, so the clock and the controls stay where the reader left them.
- **A correct flag survives a loss.** Losing shows every mine, but a square the
  player flagged right keeps its flag: taking it away punishes the one thing they
  did correctly.
- **Nothing is stored until the first move.** A board nobody touched and nobody
  flagged is removed from storage rather than saved, so an app opened once and
  abandoned leaves no trace.
- **The stored game is validated, not trusted.** `Game::from_stored` refuses a
  wrong field count, an unknown level, a status written by a version that did not
  have it, a board whose letters do not parse, more flags than mines and a first
  click off the board; anything it refuses is a fresh board, never a failed start.
- **A big board is one HTML string, not ten thousand elements.** A 99x99 board
  is 9801 squares; three retained handles each is thirty thousand objects, and
  appending them one at a time took 17 seconds on this machine (a document
  fragment alone only got it to five). The board is now written as markup built
  from `Game::cells` and parsed once — the first paint after it is a no-op by
  construction — and a square is looked up by index only when something about it
  changes. The paint's cache is the class string alone, because the glyph and the
  spoken name are functions of it.
- **There is no full-screen control and no zoom of the app's own.** There was
  both, briefly. The button called `requestFullscreen`, which is the only way to
  hide the browser's chrome — and it was not worth its place once the board
  filled the screen anyway: on a phone it bought the browser's chrome and cost a
  control, and the browser's own full screen is a menu item away. The zoom went
  because the browser's pinch is better on every axis. Nothing in the app claims
  `touch-action: none`, so nothing takes either gesture away.
- **A phone's `contextmenu` is not a right click.** Android raises one about half
  a second into a long press — *before* this app's own 600ms timer. Acting on it
  flags the square and then the timer fires and unflags it, so a long press
  appears to do nothing. The handler is gated on `press_armed()` as well as on
  the post-fire suppression window; a real right click arms no press at all
  (`pointerdown` returns early for a secondary button), which is the distinction.
- **Every board's floor is 24px.** A dense board's used to be 14, which is not a
  square a thumb can aim at. A dense board is always bigger than the screen, so a
  smaller floor never made it fit — it only made it unhittable.
- **Full screen is the browser's, and the app has no zoom of its own.** A CSS
  fold that hid the app's own chrome was worse than useless: it took away the
  browser's pinch-zoom along with the browser's chrome, and then had to grow a
  zoom of its own to put back what it had removed — a zoom that relaid out nine
  thousand squares per frame and took two seconds a frame on a 99×99 board.
  `request_fullscreen` on `<html>` gives the display *and* leaves the browser's
  zoom intact, which is better than anything a canvas of buttons can draw. The
  control is a button, not a checkbox, because full screen is browser state that
  Escape and the system can also change; its name is read from
  `document.fullscreenElement` and repainted on `fullscreenchange`, never
  remembered in a flag.
- **`touch-action: manipulation` on the board and its squares.** Not `none` —
  that stops the browser panning a board bigger than the screen — and not left
  off, because the browser's pinch-zoom is the feature now.
- **The board is a layout size, not a transform.** Zoom multiplies one custom
  property (`--zoom`) that the square size derives from, so the grid, the gaps
  and the glyph all follow from it and the numbers stay crisp. A `transform`
  would paint a bigger picture of a small one.
- **The fit divides measured pixels, not percentages.** `--fit-w`/`--fit-h` feed
  `grid-template-columns`, and a percentage there resolves against a width the
  columns themselves decide — the browser treats it as zero and the board
  collapses to its floor. `fit_canvas` measures the box and writes
  `--canvas-w`/`--canvas-h`; a `ResizeObserver` on the wrapper keeps them right
  through a fold, a rotation and a level change, reading the size from the
  observer's own entry rather than the DOM inside the callback (which is what
  makes a resize loop).
- **The page is one screen and never scrolls.** `height: 100dvh` and
  `overflow: hidden`, with the board's box the only flexible item, bounded to a
  square of the space (`--wrap-h`) so a portrait phone does not get a band of
  nothing between the board and the controls.
- **A long press is 600ms, not 450.** The board always scrolls now, so a pan
  starts on a square and every pan begins with a press; at 450ms a deliberate pan
  could cross the threshold before the finger moved, and flag a square mid-pan.
  The drift slop came down to 6px for the same reason.
- **The status line reports changes, not arithmetic.** "That 2 has only 1 of the
  2 flags it needs" was removed for the same reason: the reader put those flags
  there and can see how many there are. Only a chord that *could not run* is
  silent; one that runs and loses still ends the game, and that is reported.
- **Geometry is fluid, not computed.** The board's width is `100%` clamped
  between `cols × 24px` and `cols × 40px`, the columns are `minmax(0, 1fr)`, and
  each square is `aspect-ratio: 1`. An earlier version derived the square size
  from `100vw` minus the gaps and the padding it forgot to count, and the last
  column hung off the edge of a phone by two pixels.
- **The glyph is sized by container query units** (`52cqmin` of the square), with
  a plain `rem` fallback first. The unit has to be on the *glyph*: an element
  cannot query its own container, and `cqmin` on the button silently resolves
  against the viewport, drawing a 200px digit in a 38px square.

## Verification

```
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --lib --target wasm32-unknown-unknown -- -D warnings
cargo test --locked
```

`tests/shell.rs` holds the invariants of the shell and the two workflows: one
script tag, one `__VERSION__`, no absolute URLs, the icon's digest, the worker's
`ASSETS` allowlist against what `build.rs` publishes, both site checks identical,
every action pinned to a SHA, and the light scheme changing colours only.

The rules themselves are tested in `src/minefield.rs` and `src/game.rs` — the
safe first click at corners, edges and the middle on all three boards, the
neighbour counts against a plain count, floods, the flag ceiling, win and loss,
the clock's cap, the prose, and every malformed stored game.

**Full screen verified in a real browser on 2026-10-24**: a genuine click enters
full screen (`document.fullscreenElement` is `<html>`), the board fills the
measurements the browser reports, playing still works inside it, the control
reads "Leave full screen" and is marked pressed, clicking again leaves, and
Escape leaves *and* repaints the name — which is the case a remembered flag would
get wrong.

**Gestures and chording, verified in the same browser harness on 2026-10-24**:
a tap opens; a long press flags exactly once (not flag-then-open, which is what
the unsuppressed `click` did); a long press on a flagged square removes the flag;
in flag mode a tap flags and a long press opens; a 70px drag across five
`pointermove`s does neither; a chord on a satisfied number opens its ring; and a
chord over a wrong flag loses the game and draws the mine that ended it. Each of
those has a domain or source-text test behind it, and each was mutation-tested —
reverting the suppression, the mode swap, the chord's loss and the paint's skip
turns the corresponding test red.

**Verified in a browser on 2026-10-24** (headless Chromium over raw CDP, since
there is no node on this host): the board renders and plays at 390×844, 320×640,
768×1024 and 1280×800; no layout overflow at any of them; a first click never
loses; a real game was played to a win by reading the numbers off the page; a
loss shows all ten mines and keeps the flags; the game survives a reload; the
clock advances one second per second; the light and dark schemes both paint; a
**cold start with the network dead in a new browser process** loads the app from
the cache and plays a move; and a redeploy reaches an installed app in one cycle,
deleting the old cache and settling without a reload loop.

One inherited behaviour, from the template's service worker: a worker's
`cache.addAll` fetches through the browser's HTTP cache, so a host that lets HTML
be cached without revalidation can hand a new worker the *previous* page. With
`Cache-Control: no-cache` on HTML — or once that entry expires — the handover is
clean. Nothing in this app's code changes that; it is the family's worker.
