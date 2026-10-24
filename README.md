# Minesweeper

The board you already know: three classic sizes, flags, a clock, and a first
click that can never lose.

The whole app is Rust — the mine placement, the first click's safe zone, the
flood reveal, the flag count, the clock and the saved game. `cargo build` writes
a publishable static site into `dist/`. There is no server and no account, and
nothing leaves your machine.

A rewrite of [`wdomitrz/minesweeper`](https://github.com/wdomitrz/minesweeper),
which is a licence and nothing else, on the pattern of
[`bot-git-ai/sudoku`](https://github.com/bot-git-ai/sudoku).

## Build

Two steps, because there are two targets. Nothing generated is committed.

```bash
rustup target add wasm32-unknown-unknown
cargo install wasm-bindgen-cli --version 0.2.128 --locked

# 1. the app: compile to wasm and generate the bindings
cargo build --locked --lib --target wasm32-unknown-unknown --release
wasm-bindgen --target web --no-typescript --out-dir dist --out-name app \
  target/wasm32-unknown-unknown/release/minesweeper.wasm

# 2. the rest of the site
touch build.rs
cargo build --release --locked
```

That gives you a publishable `dist/`. **The order matters**: step 2 derives the
service worker's cache name from the wasm, so running it first would pin that to
the previous build. The `touch` matters too — `build.rs` writes into the source
tree, not `OUT_DIR`, so cargo will not re-run it for a second identical
invocation.

`cargo test --locked` needs none of the above: it runs without the wasm target
or the bindings generator.

## Use it

`dist/` is self-contained and everything in it is relative, so it mounts under
any path. Point any file host at it — nginx, Caddy, GitHub Pages,
`python3 -m http.server` — and open the result.

Tap a square to open it. The first square you open is always safe: the mines are
placed after that tap, and neither it nor its neighbours are mined, so a board
can never be lost before it has been played. A square with no mines around it
opens everything reachable from it. The counter above the board is the mines not
yet flagged, and the clock starts on the first square you open rather than when
the page loads.

**The board is the app.** It takes the whole screen: the title and the clock sit
above it, the controls below, and the board gets everything in between. A board
bigger than the screen — Expert and Insane are — is panned by dragging, and your
browser's own pinch-zoom magnifies it.

**A long press does the other thing.** Hold a square to flag it; hold one while
the board is in flag mode and it opens instead. Neither mode costs a trip to the
button, and a press that turns into a scroll does nothing.

**Tap a number whose flags are all placed to open everything around it.** That is
what the numbers are for, and it is the fastest way to play — but the flags are
your claim about the board, not the board itself, so a chord over a flag in the
wrong place will open the mine it was hiding. The counter going down is not the
same as being right.

Press **Flag** to switch to marking with plain taps, or right-click on a desktop.

Four sizes: Beginner is 9×9 with 10 mines, Intermediate 16×16 with 40, Expert
30×16 with 99 and Insane 99×99 with 2000. Expert is the board the game has always
been and Insane is absurd on purpose, so on a phone both scroll instead of being
squeezed into untappable squares.

A game in progress is kept, including one you lost, so closing the tab or losing
signal does not lose the board. The clock is wall-clock time spent with the game
being played: a tab left in the background for an hour still reports the hour,
because you were not looking at it and the clock was.

## Accessibility

Every square is a real button with a spoken name that says where it is and what
it is — "Row 4, column 7, 3 mines around" — because a square's visible content is
one digit and a digit alone never says which of 480 squares it is. One square is
in the tab order at a time and the arrow keys move it, so a keyboard reader does
not have to tab through the whole board. What just happened is announced once, on
a live region, rather than on every tick of the clock.

## Layout

The board is square by construction: its width is the available width clamped
between 24 and 40 pixels a column, the columns share that width as equal
fractions, and each square takes its height from its own width. Below 24 pixels a
finger cannot pick a square, so a board that does not fit scrolls instead of
shrinking.

One palette in two schemes, dark by default and light when the system asks for
it, decided in CSS alone so the right one is painted before the first frame.
