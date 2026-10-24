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

- **Flag mode is a button, not a gesture.** A phone has no right button and a
  long press is undiscoverable; a checkbox would also toggle without a listener
  but cannot be *disabled* when the game is over, and flagging a finished board
  is meaningless. A button carries `aria-pressed` for the same information.
- **One tab stop, arrows to move.** 480 buttons in the tab order is not
  navigation. `roving tabindex`, with the arrow keys bounded by the board rather
  than wrapping around it.
- **The clock is a wall clock, and it carries its remainder.** An interval fires
  0.999s apart as often as 1.001s; dropping the fraction, or comparing it to one
  second, stops the clock dead. That bug shipped into the first build and was
  caught by playing the game in a browser — the unit tests pass a whole number of
  seconds, so no unit test could have seen it. Time is only banked while the game
  is being played.
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
