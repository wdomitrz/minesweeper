//! The browser layer: the only file that talks to the DOM.
//!
//! Every decision — which squares are mined, what is covered, what is flagged,
//! what the clock says, how the game ended — was made in [`crate::minefield`]
//! and [`crate::game`], and is unit tested there. What is left here is element
//! handles, attribute writes, listeners and a timer, and nothing that could
//! have been written down as a rule.
//!
//! Three things this file does deliberately:
//!
//! * **One listener per board, not one per square.** An Expert board is 480
//!   buttons; a closure each would be 480 forgotten closures and 480 chances
//!   for one to be attached twice. The board has a single click listener that
//!   reads the square's index off the element the event landed on.
//! * **A square is built once and then only described.** Rebuilding 480 buttons
//!   on every render would throw away focus and restart any tap in progress, so
//!   the elements are made when the board is made, and a render rewrites only
//!   what changed.
//! * **The clock redraws the clock.** A second passing changes one line, so
//!   [`App::tick`] paints the facts and leaves the board alone.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    Document, Element, Event, HtmlButtonElement, HtmlSelectElement, KeyboardEvent, PointerEvent,
    Storage, StorageManager, Window,
};

use crate::game::{Cell as Square, Game, Report, Status, STORAGE_KEY};
use crate::minefield::Level;

/// The app's whole mutable state, and the element handles it writes through.
///
/// `Rc<RefCell<_>>` because wasm has no threads and every handler below is a
/// plain closure rather than a `'static` thread. The rule that keeps this
/// honest: a handler borrows, mutates, drops the borrow, then renders. It never
/// renders from inside a borrow.
type Shared = Rc<RefCell<App>>;

/// The scope this app's worker is registered for.
///
/// Stated rather than inherited, and the one string that has to agree with
/// `src/service-worker.js`: the worker resolves its own directory the same way,
/// from `self.location`.
const SCOPE: &str = "./";

/// How often the clock is checked. It is a check, not a tick: the elapsed time
/// comes from the wall clock, so a tab the browser throttled still catches up.
const TICK_MS: i32 = 1_000;

/// How long a press has to last to count as a long press, in milliseconds.
///
/// 450 is the usual bar: comfortably longer than a tap (which is over in well
/// under 200ms even when deliberate) and short enough that a reader who is
/// holding on purpose does not think it failed. The other half of recognising a
/// long press is what it must *not* be: a scroll, a drag, or a press that ends
/// before the timer fires.
const LONG_PRESS_MS: i32 = 450;

/// How long after a long press the browser's trailing events are ignored, in
/// milliseconds.
///
/// Longer than the gap between the press and the `click` on release, and longer
/// than Android's own `contextmenu` delay, with room for a laggy device. Too long
/// only means a deliberate tap immediately after a long press is dropped, which
/// is not a gesture anyone makes on purpose.
const SUPPRESS_MS: i32 = 700;

/// Above this many squares, a board is drawn dense: no lighting per square and a
/// one-pixel gap, because the alternative is 9801 little raised buttons which
/// reads as woven fabric rather than as a board.
const DENSE_CELLS: usize = 2_000;

/// How far a finger may drift, in CSS pixels, before a press stops being a press.
///
/// A thumb resting on a square moves a little; a finger that is scrolling moves
/// a lot. This is the line between the two, and it is generous enough that a
/// press does not fail for being held by a human being.
const PRESS_SLOP_PX: f64 = 10.0;

/// Start the app.
///
/// Runs automatically when the page's dynamic import resolves, because the
/// loader in `ui.html` is `import('./app.js').then(m => m.default())` and
/// `default` is this start function.
///
/// # Errors
///
/// Fails only when the document is not there at all — a browser without a
/// `window` or a `document` is not a page this app can run in. A missing
/// element inside the shell degrades a feature rather than failing startup.
#[wasm_bindgen(start)]
pub fn start() -> Result<(), JsValue> {
    let window = web_sys::window().ok_or_else(|| JsValue::from_str("no window"))?;
    let document = window
        .document()
        .ok_or_else(|| JsValue::from_str("no document"))?;

    let app: Shared = Rc::new(RefCell::new(App::build(&window, &document)?));
    {
        let mut app = app.borrow_mut();
        app.paint();
        // A game restored from a previous visit may already be over. The status
        // line is the only place that says so, and it says it in the same
        // words a live game uses, because both come from the same function.
        let summary = app.game.readout().summary;
        if !summary.is_empty() {
            set_text(&app.document, "status", &summary);
        }
    }
    bind(&document, &app);
    start_clock(&window, &app);
    announce_version();

    // By value, not by reference: the future outlives this function, so it has
    // to own what it holds. This is the same rule the event closures follow.
    let offline = window.clone();
    spawn_local(keep_offline(offline));
    Ok(())
}

/// Everything one running app holds.
struct App {
    window: Window,
    document: Document,
    game: Game,
    /// The board element, which is every square's parent.
    ///
    /// There is deliberately no `Vec` of ten thousand button handles. A board
    /// can be 99 squares square, and holding three element handles per square —
    /// the button, its glyph and its spoken name — costs more to build than the
    /// board itself does. A square is looked up by its index when something
    /// about it changes, which for one click is a handful of squares and for a
    /// flood is the ones that opened.
    board: Option<Element>,
    /// The classes last written to each square, so a render can skip the ones
    /// that did not change: opening one square must not rewrite 480 of them.
    /// The glyph and the spoken name are derived from this string, so this one
    /// cache covers all three.
    shown: Vec<String>,
    /// Which square currently carries the tab stop, so a repaint moves it only
    /// when it moved.
    shown_focus: Option<usize>,
    /// The square the arrow keys move from. Only one square is in the tab
    /// order at a time, or a keyboard reader would have to tab 480 times.
    focused: usize,
    /// Whether the next tap marks a square instead of opening it.
    flag_mode: bool,
    /// The wall clock reading at the previous tick, in milliseconds.
    last_tick: Cell<f64>,
    /// The press in progress: the square it started on, where the finger was,
    /// and the timer that would make it a long press.
    press: Cell<Option<Press>>,
    /// When the last long press finished, as a wall-clock time.
    ///
    /// A long press does its own work, and the browser then sends more events
    /// for the same gesture: a `click` on release, and — on Android — a
    /// `contextmenu` about half a second in. Both would do the *other* action,
    /// so both are ignored while this time has not passed. A time rather than a
    /// flag because the two arrive in an order that is not the same on every
    /// platform, and a flag consumed by the first would let the second through.
    long_press_until: Cell<f64>,
    /// Seconds the clock has measured but not yet handed to the game.
    ///
    /// An interval fires *approximately* every second, so the delta is as often
    /// 0.999 as it is 1.001. Discarding the fraction — or handing the game a
    /// rounded zero — stops the clock dead, which is exactly what an earlier
    /// version did: it compared the delta to one second and returned, so on this
    /// machine, whose timers run a shade early, the game showed 00:00 forever.
    /// The remainder is carried instead, so tiny early and late ticks average
    /// out to the right number of seconds.
    pending: Cell<f64>,
}

impl App {
    /// Build the app: restore a game if there is one, otherwise start a fresh
    /// board, and create the elements that are created once.
    fn build(window: &Window, document: &Document) -> Result<Self, JsValue> {
        let game = stored_game(window).unwrap_or_else(|| Game::new(Level::Beginner, fresh_seed()));
        let mut app = Self {
            window: window.clone(),
            document: document.clone(),
            game,
            board: None,
            shown: Vec::new(),
            shown_focus: None,
            focused: 0,
            flag_mode: false,
            last_tick: Cell::new(js_sys::Date::now()),
            pending: Cell::new(0.0),
            press: Cell::new(None),
            long_press_until: Cell::new(0.0),
        };
        app.build_board()?;
        Ok(app)
    }

    /// Make one button per square, replacing whatever the previous board left.
    ///
    /// The board is built as **one HTML string** and parsed once, rather than
    /// element by element. Appending ten thousand buttons one at a time took 17
    /// seconds on this machine for the biggest board — every `append_child` on a
    /// live element invalidates style and layout for the whole board — and a
    /// document fragment alone only brought it to five. A single `innerHTML` is
    /// parsed natively and takes a fraction of a second.
    ///
    /// The markup is generated from [`Game::cells`], the same source the paint
    /// uses, so what the string says and what a repaint would write cannot
    /// drift: the first paint after this is a no-op, by construction.
    ///
    /// Nothing a reader typed reaches this string — it is digits, fixed class
    /// names and fixed words — which is why it needs no escaping.
    fn build_board(&mut self) -> Result<(), JsValue> {
        let board =
            element(&self.document, "board").ok_or_else(|| JsValue::from_str("no #board"))?;
        let level = self.game.level();
        board.set_attribute(
            "style",
            &format!("--cols:{};--rows:{}", level.width(), level.height()),
        )?;
        // A board this size is drawn dense: smaller squares, a smaller gap, and
        // no per-square lighting, because 9801 gradients is a texture rather
        // than a set of buttons. The flag is the level's own property, not a
        // measurement of the screen: the same level looks the same everywhere.
        if level.cells() > DENSE_CELLS {
            board.set_attribute("data-dense", "true")?;
        } else {
            board.remove_attribute("data-dense")?;
        }

        let cells = self.game.cells();
        board.set_inner_html(&board_html(&cells)?);

        // The cache is primed with exactly what was just written, so the first
        // paint finds nothing to do. A board restored from a saved game comes
        // back with its flags and open squares already on it, for the same
        // reason: the markup is the game's own state, not a fresh blank board.
        self.shown = cells.iter().map(Square::classes).collect();
        self.shown_focus = None;
        self.focused = 0;
        self.board = Some(board);
        Ok(())
    }

    /// Paint everything.
    fn paint(&mut self) {
        self.paint_board();
        self.paint_facts();
        self.paint_tools();
    }

    /// The button for one square, looked up when it is needed.
    fn button(&self, index: usize) -> Option<HtmlButtonElement> {
        let index = u32::try_from(index).ok()?;
        self.board.as_ref()?.children().item(index)?.dyn_into().ok()
    }

    /// Paint every square whose appearance changed, and the tab stop.
    ///
    /// The class string is the single source of what a square looks like: the
    /// glyph and the spoken name are *functions* of it, so a square whose classes
    /// are unchanged has nothing else to update either. That is what keeps a
    /// 99x99 board usable — one click opens a flood of squares and must not cost
    /// a hundred thousand DOM writes for the nine thousand it did not touch.
    fn paint_board(&mut self) {
        let cells = self.game.cells();
        if cells.len() != self.shown.len() {
            // The board was not rebuilt after a level change. Painting what is
            // there would label squares with the wrong level's names.
            warn(
                "the board does not match the game",
                &JsValue::from_str("rebuild it"),
            );
            return;
        }
        if let Some(board) = element(&self.document, "board") {
            let _ = board.set_attribute("data-status", self.game.status().slug());
        }
        for (index, cell) in cells.iter().enumerate() {
            let classes = cell.classes();
            if self.shown.get(index) == Some(&classes) {
                continue;
            }
            let Some(button) = self.button(index) else {
                continue;
            };
            button.set_class_name(&classes);
            // The button's two children in the order `board_html` writes them:
            // the glyph, then the spoken name.
            if let (Some(face), Some(label)) =
                (button.first_element_child(), button.last_element_child())
            {
                face.set_text_content(Some(&cell.face()));
                label.set_text_content(Some(&cell.label()));
            }
            if let Some(slot) = self.shown.get_mut(index) {
                *slot = classes;
            }
        }

        // The tab stop moves only when the reader moves it: writing `tabindex`
        // on ten thousand buttons per repaint is what the loop above exists to
        // avoid, and it would be the whole cost of it back again.
        if self.shown_focus != Some(self.focused) {
            if let Some(previous) = self.shown_focus {
                if let Some(button) = self.button(previous) {
                    let _ = button.set_attribute("tabindex", "-1");
                }
            }
            if let Some(button) = self.button(self.focused) {
                let _ = button.set_attribute("tabindex", "0");
            }
            self.shown_focus = Some(self.focused);
        }
    }

    /// Paint the clock and the counter.
    fn paint_facts(&mut self) {
        let readout = self.game.readout();
        set_text(&self.document, "clock", &readout.clock);
        set_text(&self.document, "remaining", &readout.remaining);
    }

    /// Paint the controls whose state is part of the game.
    fn paint_tools(&mut self) {
        if let Some(flag) = button(&self.document, "flag") {
            let _ = flag.set_attribute(
                "aria-pressed",
                if self.flag_mode { "true" } else { "false" },
            );
            // Flagging is meaningless once the game is over, and a control
            // that does nothing is better disabled than silently inert.
            flag.set_disabled(self.game.is_finished());
        }
        if let Some(select) = self
            .document
            .get_element_by_id("level")
            .and_then(|element| element.dyn_into::<HtmlSelectElement>().ok())
        {
            let wanted = self.game.level().slug();
            if select.value() != wanted {
                select.set_value(wanted);
            }
        }
    }

    /// Advance the clock and repaint the facts only.
    ///
    /// `seconds` is a wall-clock delta, so a tab the browser throttled to one
    /// timer call a minute still reports the minute it actually spent on the
    /// game. The game clamps what one call may add.
    fn tick(&mut self, seconds: u32) {
        self.game.tick(seconds);
        self.paint_facts();
    }

    /// Open the square at `index`, or chord on it when it is already open and
    /// numbered.
    ///
    /// Whether that is a chord is [`Game::open`]'s decision, not this layer's:
    /// the page does not get to decide a rule from class names it just wrote.
    fn open(&mut self, index: usize) {
        self.focused = index;
        let report = self.game.open(index);
        self.after(report);
    }

    /// Mark the square at `index`, or unmark it.
    fn flag(&mut self, index: usize) {
        self.focused = index;
        let report = self.game.flag(index);
        self.after(report);
    }

    /// What a tap does: open (or chord) in flag mode, flag otherwise.
    fn tap(&mut self, index: usize) {
        if self.flag_mode {
            self.flag(index);
        } else {
            self.open(index);
        }
    }

    /// What a long press does: the *other* action, whichever one that is.
    ///
    /// It is deliberately not "always flag". A long press that did the same
    /// thing whatever mode the board is in would be a second way to flag, and
    /// the point of it is that neither mode costs a trip to the Flag button:
    /// while flagging, a long press opens; while opening, it flags.
    fn long_press(&mut self, index: usize) {
        if self.flag_mode {
            self.open(index);
        } else {
            self.flag(index);
        }
        self.buzz();
    }

    /// Whether a long press has just finished, and the events the browser sends
    /// for the rest of that gesture are therefore not new gestures.
    ///
    /// Reading it does not consume it: `click` and `contextmenu` both arrive
    /// after one press, and whichever comes first must not let the other
    /// through.
    fn long_press_finished(&self) -> bool {
        js_sys::Date::now() < self.long_press_until.get()
    }

    /// A short vibration, where the browser offers one.
    ///
    /// Only a confirmation that the press has been taken — there is no other
    /// feedback on a phone that a long press has fired, and a reader who is
    /// unsure presses again, which flags and then unflags the same square.
    /// A refusal is a normal condition: iOS has no `vibrate` at all.
    fn buzz(&self) {
        let _ = self.window.navigator().vibrate_with_duration(12);
    }

    /// Say what just happened, repaint and save.
    fn after(&mut self, report: Report) {
        let words = report.words();
        if !words.is_empty() {
            set_text(&self.document, "status", &words);
        }
        self.paint();
        self.save();
    }

    /// Start a new game on `level`.
    fn new_game(&mut self, level: Level) {
        // A press armed on the old board must not fire against the new one.
        self.cancel_press();
        self.game = Game::new(level, fresh_seed());
        self.flag_mode = false;
        self.last_tick.set(js_sys::Date::now());
        self.pending.set(0.0);
        if let Err(error) = self.build_board() {
            warn("the board could not be rebuilt", &error);
        }
        set_text(&self.document, "status", "");
        self.paint();
        self.save();
    }

    /// Stop the press in progress, if there is one.
    ///
    /// Called on release, on the browser's cancellation, on drift, and before a
    /// new press starts — so a timer can never fire against a square the reader
    /// has already moved away from.
    fn cancel_press(&mut self) {
        if let Some(press) = self.press.take() {
            self.stop_timer(press.timer);
        }
    }

    /// Stop a timer that has not fired.
    fn stop_timer(&self, timer: i32) {
        if let Some(window) = web_sys::window() {
            window.clear_timeout_with_handle(timer);
        }
    }

    /// Flip between opening squares and marking them.
    fn toggle_flag_mode(&mut self) {
        self.flag_mode = !self.flag_mode;
        self.paint_tools();
    }

    /// Move the tab stop and the keyboard focus to `index`.
    ///
    /// Two writes, not ten thousand: `paint_board` owns the tab-stop bookkeeping,
    /// and the browser moves focus itself.
    fn focus(&mut self, index: usize) {
        if index >= self.shown.len() {
            return;
        }
        self.focused = index;
        self.paint_board();
        if let Some(button) = self.button(index) {
            let _ = button.focus();
        }
    }

    /// Persist the game, if the player has done anything worth keeping.
    ///
    /// An app the reader never touched leaves no trace: a game nobody has
    /// opened a square of and nobody has flagged is removed rather than saved.
    fn save(&self) {
        let Some(storage) = self.storage() else {
            return;
        };
        let untouched = self.game.status() == Status::Ready && self.game.flags() == 0;
        if untouched {
            let _ = storage.remove_item(STORAGE_KEY);
            return;
        }
        if let Err(error) = storage.set_item(STORAGE_KEY, &self.game.to_stored()) {
            warn("could not save the game", &error);
        }
    }

    /// The page's `localStorage`, if the browser will hand it over.
    fn storage(&self) -> Option<Storage> {
        self.window.local_storage().ok().flatten()
    }
}

/// Start the clock.
///
/// One interval for the life of the page, rather than one per game: a timer per
/// board would need cancelling on every new game and would leak the ones that
/// were not. The game itself decides what a second means — a finished game
/// ignores it.
fn start_clock(window: &Window, app: &Shared) {
    let shared = Rc::clone(app);
    let closure = Closure::<dyn FnMut()>::new(move || {
        let now = js_sys::Date::now();
        let mut app = shared.borrow_mut();
        let delta = (now - app.last_tick.get()) / 1_000.0;
        app.last_tick.set(now);

        // A game that is not being played banks nothing: time spent looking at a
        // fresh board, or at one already lost, is not game time.
        if app.game.status() != Status::Playing {
            app.pending.set(0.0);
            return;
        }
        // A clock that jumped backwards (a device with a wrong date) measures
        // nothing and is not allowed to subtract; a huge one is clamped by the
        // game itself.
        if !delta.is_finite() || delta < 0.0 {
            return;
        }
        let pending = app.pending.get() + delta;
        let whole = pending.floor();
        app.pending.set(pending - whole);
        if whole < 1.0 {
            return;
        }
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let seconds = whole.min(f64::from(u32::MAX)) as u32;
        app.tick(seconds);
    });
    if window
        .set_interval_with_callback_and_timeout_and_arguments_0(
            closure.as_ref().unchecked_ref(),
            TICK_MS,
        )
        .is_err()
    {
        warn(
            "the clock could not start",
            &JsValue::from_str("no interval"),
        );
    }
    closure.forget();
}

/// Bind every listener.
///
/// Handlers mutate the state, then repaint. That is the whole application loop:
/// no framework, no virtual DOM, no diffing.
fn bind(document: &Document, app: &Shared) {
    bind_board(document, app);
    bind_tools(document, app);
}

/// The whole board, as one string of markup.
///
/// Generated from the cells the game reports, so the first paint after building
/// a board has nothing to do: the string already says what a repaint would.
///
/// The text in it is digits, fixed class names and fixed words, so it needs no
/// escaping — nothing a reader can type reaches this function, and if something
/// ever does, this is the line that has to change.
fn board_html(cells: &[Square]) -> Result<String, JsValue> {
    use std::fmt::Write as _;
    let mut html = String::with_capacity(cells.len() * 128);
    for (index, cell) in cells.iter().enumerate() {
        write!(
            html,
            "<button type=\"button\" tabindex=\"-1\" data-index=\"{index}\" class=\"{classes}\">\
             <span class=\"face\" aria-hidden=\"true\">{face}</span>\
             <span class=\"visually-hidden\">{label}</span></button>",
            classes = cell.classes(),
            face = cell.face(),
            label = cell.label(),
        )
        .map_err(|_| JsValue::from_str("the board could not be written"))?;
    }
    Ok(html)
}

/// The board's three listeners: open, mark, and move.
fn bind_board(document: &Document, app: &Shared) {
    let Some(board) = element(document, "board") else {
        warn("no board to bind", &JsValue::from_str("no #board"));
        return;
    };

    let shared = Rc::clone(app);
    let click = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Some(index) = cell_index(&event) else {
            return;
        };
        let mut app = shared.borrow_mut();
        // A long press has already done its work and the browser still sends a
        // click after the release. Doing both would flag a square and then open
        // it, which on a covered square is a lost game.
        if app.long_press_finished() {
            return;
        }
        app.tap(index);
    });
    listen(&board, "click", click.as_ref().unchecked_ref());
    click.forget();

    bind_press(&board, app);

    // A right click marks a square: the desktop habit, and it costs one
    // listener. On a phone the long press below is the equivalent.
    let shared = Rc::clone(app);
    let mark = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Some(index) = cell_index(&event) else {
            return;
        };
        event.prevent_default();
        let mut app = shared.borrow_mut();
        // Android turns a long press into a `contextmenu` at about half a
        // second. That is the same gesture this app has already handled, so
        // acting on it again would flag the square a second time and undo the
        // flag the reader just made.
        if app.long_press_finished() {
            return;
        }
        app.flag(index);
    });
    listen(&board, "contextmenu", mark.as_ref().unchecked_ref());
    mark.forget();

    let shared = Rc::clone(app);
    let keys = Closure::<dyn FnMut(KeyboardEvent)>::new(move |event: KeyboardEvent| {
        let Some(target) = event
            .target()
            .and_then(|node| node.dyn_into::<Element>().ok())
        else {
            return;
        };
        let Some(index) = index_of(&target) else {
            return;
        };
        let mut app = shared.borrow_mut();
        let level = app.game.level();
        let Some(next) = neighbour(level.width(), level.height(), index, &event.key()) else {
            return;
        };
        event.prevent_default();
        app.focus(next);
    });
    listen(&board, "keydown", keys.as_ref().unchecked_ref());
    keys.forget();
}

/// The press that may become a long press.
///
/// Three listeners and a timer, rather than a gesture library: `pointerdown`
/// starts the timer, and `pointerup`, `pointercancel` and a `pointermove` past a
/// thumb's width of drift all cancel it. `pointercancel` is the one that matters
/// on a phone — a finger that starts scrolling is cancelled by the browser, and
/// a long press that survived that would flag a square every time the reader
/// scrolled a board.
fn bind_press(board: &Element, app: &Shared) {
    let shared = Rc::clone(app);
    let down = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        let Some(index) = cell_index(&event) else {
            return;
        };
        // A right click is not a press: it is the desktop's way to flag, and it
        // arrives here as button 2 before the `contextmenu` that handles it.
        if event.button() != 0 {
            return;
        }
        let Some(window) = web_sys::window() else {
            return;
        };
        let pressed = shared.clone();
        let timer = Closure::<dyn FnMut()>::new(move || {
            let mut app = pressed.borrow_mut();
            // The press is taken, not read: this timer has fired, so there is
            // nothing left to cancel, and the action below repaints — a record
            // of an armed press surviving a render would be a press that can
            // never end.
            let Some(press) = app.press.take() else {
                return;
            };
            // The belt to `pointerdown`'s braces: a timer that finds a press on
            // a different square has outlived its press, and must do nothing.
            if press.index != index {
                return;
            }
            app.long_press_until
                .set(js_sys::Date::now() + f64::from(SUPPRESS_MS));
            app.long_press(index);
        });
        let Ok(handle) = window.set_timeout_with_callback_and_timeout_and_arguments_0(
            timer.as_ref().unchecked_ref(),
            LONG_PRESS_MS,
        ) else {
            return;
        };
        // The closure is held by the browser only for the length of the timer,
        // so it has to be forgotten *after* it is handed over, and the handle is
        // what cancels it early.
        timer.forget();
        let mut app = shared.borrow_mut();
        app.cancel_press();
        app.press.set(Some(Press {
            index,
            x: f64::from(event.client_x()),
            y: f64::from(event.client_y()),
            timer: handle,
        }));
    });
    listen(board, "pointerdown", down.as_ref().unchecked_ref());
    down.forget();

    let shared = Rc::clone(app);
    let up = Closure::<dyn FnMut(PointerEvent)>::new(move |_| {
        shared.borrow_mut().cancel_press();
    });
    listen(board, "pointerup", up.as_ref().unchecked_ref());
    up.forget();

    // The browser's own cancellation: a scroll started, a second finger
    // arrived, the pointer left the window. Any of them means this was not a
    // press, and none of them will send a `pointerup`.
    let shared = Rc::clone(app);
    let cancel = Closure::<dyn FnMut(Event)>::new(move |_| {
        shared.borrow_mut().cancel_press();
    });
    listen(board, "pointercancel", cancel.as_ref().unchecked_ref());
    cancel.forget();

    let shared = Rc::clone(app);
    let drift = Closure::<dyn FnMut(PointerEvent)>::new(move |event: PointerEvent| {
        let app = shared.borrow_mut();
        // The press is taken out and put back, because `Press` is not `Copy` and
        // borrowing it would fight the `cancel_press` call below. Anything that
        // leaves here without putting it back must have stopped its timer.
        let Some(press) = app.press.take() else {
            return;
        };
        let moved =
            (f64::from(event.client_x()) - press.x).hypot(f64::from(event.client_y()) - press.y);
        if moved > PRESS_SLOP_PX {
            // Too far to be a press on one square: the reader is scrolling, and
            // a long press would fire on a square their finger has left.
            app.stop_timer(press.timer);
            return;
        }
        app.press.set(Some(press));
    });
    listen(board, "pointermove", drift.as_ref().unchecked_ref());
    drift.forget();
}

/// A press that may still become a long press.
struct Press {
    /// The square the finger went down on.
    index: usize,
    /// Where it went down, in viewport pixels, for the drift check.
    x: f64,
    y: f64,
    /// The timer to stop if the press ends early.
    timer: i32,
}

/// The three controls: a mode, a level and a new game.
fn bind_tools(document: &Document, app: &Shared) {
    if let Some(flag) = button(document, "flag") {
        let shared = Rc::clone(app);
        let toggle = Closure::<dyn FnMut()>::new(move || {
            shared.borrow_mut().toggle_flag_mode();
        });
        listen(&flag, "click", toggle.as_ref().unchecked_ref());
        toggle.forget();
    }

    if let Some(new) = button(document, "new") {
        let shared = Rc::clone(app);
        let restart = Closure::<dyn FnMut()>::new(move || {
            let mut app = shared.borrow_mut();
            let level = app.game.level();
            app.new_game(level);
        });
        listen(&new, "click", restart.as_ref().unchecked_ref());
        restart.forget();
    }

    if let Some(select) = document
        .get_element_by_id("level")
        .and_then(|element| element.dyn_into::<HtmlSelectElement>().ok())
    {
        let shared = Rc::clone(app);
        let change = Closure::<dyn FnMut(Event)>::new(move |_| {
            let mut app = shared.borrow_mut();
            let Some(select) = app
                .document
                .get_element_by_id("level")
                .and_then(|element| element.dyn_into::<HtmlSelectElement>().ok())
            else {
                return;
            };
            // A level this build does not know is not a level: leave the game
            // as it is rather than starting a board nobody asked for.
            let Some(level) = Level::from_slug(&select.value()) else {
                return;
            };
            if level == app.game.level() {
                return;
            }
            app.new_game(level);
        });
        listen(&select, "change", change.as_ref().unchecked_ref());
        change.forget();
    }
}

/// Register the worker, and hand it a reload when a new version is ready.
///
/// # Errors
///
/// When the browser refuses the registration: no worker support, a scope it
/// will not grant, or a script that fails to fetch. Offline support is then
/// absent and the app is otherwise unaffected, which is why this is a warning
/// rather than a failed start.
async fn keep_offline(window: Window) {
    match register_worker(&window).await {
        Ok(container) => release_stale_registrations(&window, &container).await,
        Err(error) => warn("offline support is unavailable", &error),
    }
    keep_storage(&window).await;
}

/// Ask the browser not to evict this app's cached files.
///
/// Cache Storage is evictable by default: under storage pressure a browser may
/// drop the copy that makes the app work offline, and the reader finds out on a
/// train. The request is a request — a browser may decline, and some do — so a
/// refusal is a normal condition and not something to report on the page.
async fn keep_storage(window: &Window) {
    let storage: StorageManager = window.navigator().storage();
    let Ok(promise) = storage.persist() else {
        return;
    };
    let _ = JsFuture::from(promise).await;
}

async fn register_worker(window: &Window) -> Result<web_sys::ServiceWorkerContainer, JsValue> {
    let container = window.navigator().service_worker();

    // The scope is stated rather than inherited: left to itself, a
    // registration's scope is the directory of the page that registered it —
    // right today, and silently wrong once the app is published elsewhere. A
    // worker registered for the whole origin answers for every page on it,
    // including pages that are not this app.
    let options = web_sys::RegistrationOptions::new();
    options.set_scope(SCOPE);
    let registration =
        JsFuture::from(container.register_with_options("./service-worker.js", &options))
            .await?
            .dyn_into::<web_sys::ServiceWorkerRegistration>()?;

    // A worker that was already waiting — a deploy that happened while the app
    // was closed — is the first case, and the one a phone hits most often.
    ask_waiting_worker_in(&registration);

    // The second case: this visit finds the update. `updatefound` fires, the
    // installing worker reaches `installed`, and only then is it waiting.
    let registration_for_update = registration.clone();
    let update_found = Closure::<dyn FnMut()>::new(move || {
        let Some(installing) = registration_for_update.installing() else {
            return;
        };
        let waiting_registration = registration_for_update.clone();
        // The listener needs the worker too, so it gets its own handle rather
        // than borrowing the one the attachment below consumes.
        let watched = installing.clone();
        let on_state = Closure::<dyn FnMut()>::new(move || {
            if watched.state() == web_sys::ServiceWorkerState::Installed {
                ask_waiting_worker_in(&waiting_registration);
            }
        });
        let _ = installing
            .add_event_listener_with_callback("statechange", on_state.as_ref().unchecked_ref());
        // The closure has to outlive the listener it was given to. It is fired
        // at most twice (installing → installed) and then replaced on the next
        // update, so holding it is the point rather than a leak worth avoiding.
        on_state.forget();
    });
    let _ = registration
        .add_event_listener_with_callback("updatefound", update_found.as_ref().unchecked_ref());
    update_found.forget();

    // …and, because the two cases above are both races the browser can lose, a
    // check of our own. `register()` resolving does not mean the update check it
    // started has finished, so an update that arrives a moment later fires
    // `updatefound` before the listener above exists — the new worker then sits
    // in `waiting` for as long as the app stays open, which is exactly how an
    // installed app stops receiving fixes. Asking for a check and looking again
    // when it resolves closes that window.
    if let Ok(promise) = registration.update() {
        let _ = JsFuture::from(promise).await;
    }
    ask_waiting_worker_in(&registration);

    // And the handover itself: when the new worker takes control, the page is
    // still the old version in memory, so reload — exactly once, or the page
    // would loop.
    let reloaded = Rc::new(Cell::new(false));
    let window = window.clone();
    let controller = Closure::<dyn FnMut()>::new(move || {
        if reloaded.replace(true) {
            return;
        }
        let _ = window.location().reload();
    });
    let _ = container
        .add_event_listener_with_callback("controllerchange", controller.as_ref().unchecked_ref());
    controller.forget();

    Ok(container)
}

/// Hand this app's own URLs back to the current worker.
///
/// A registration is kept by the browser, not by the tab, so it outlives the
/// page that made it and keeps answering for its scope until something
/// explicitly unregisters it. That is how a page on this origin comes to be
/// served by a worker installed for a *different* app, long after that app was
/// closed — and a reload, a newer version or a newer worker cannot fix it,
/// because the newer worker only takes control where its own scope reaches.
///
/// So: unregister any registration whose scope covers this app's directory but
/// is not this app's directory. Failures are ignored — this is best-effort
/// cleanup of state this app did not create, and a browser that refuses leaves
/// the reader no worse off.
async fn release_stale_registrations(window: &Window, container: &web_sys::ServiceWorkerContainer) {
    let Ok(registrations) = JsFuture::from(container.get_registrations()).await else {
        return;
    };
    let Ok(array) = registrations.dyn_into::<js_sys::Array>() else {
        return;
    };

    // This app's own directory, as an absolute URL with a trailing slash.
    let Ok(home) = window.location().href() else {
        return;
    };
    let Ok(ours) = web_sys::Url::new_with_base(&home, "./") else {
        return;
    };
    let ours = ours.href();

    for entry in array.iter() {
        let Ok(registration) = entry.dyn_into::<web_sys::ServiceWorkerRegistration>() else {
            continue;
        };
        let scope = registration.scope();
        // Leave alone any scope that is this app's own, or narrower: a sibling
        // app mounted inside this directory is legitimate and separate, and
        // nothing there can intercept us. One test, because the two cases are
        // the same one: `ours` begins with `scope`.
        if ours.starts_with(&scope) {
            continue;
        }
        // What is left is a scope that is a *strict* prefix of ours: a worker
        // that would be consulted for this app's URLs while registered for more
        // of the origin. Only our own worker qualifies — a different app's
        // worker lives in a different directory, so unregistering it would break
        // the app it belongs to.
        let script = registration
            .active()
            .map(|worker| worker.script_url())
            .unwrap_or_default();
        if script_belongs_to_app(&script, &ours) {
            match registration.unregister() {
                Ok(promise) => {
                    let _ = JsFuture::from(promise).await;
                }
                Err(error) => warn("stale service worker could not be released", &error),
            }
        }
    }
}

/// Whether the worker script at `script` is this app's own worker, registered
/// for more of the origin than this app's directory.
///
/// A wider scope means the script sits at the root of this app's own directory
/// rather than below it: a sibling app's worker is in a sibling directory and
/// does not match. `strip_suffix`, not `trim_end_matches` — the latter strips a
/// *set of characters*, so it would eat a directory named `...e-worker.js` and
/// call it ours.
fn script_belongs_to_app(script: &str, ours: &str) -> bool {
    match web_sys::Url::new(script) {
        Ok(url) => url.href().strip_suffix("service-worker.js") == Some(ours),
        Err(_) => false,
    }
}

/// Ask a waiting worker to take over.
///
/// The message is the whole protocol: the worker's `skipWaiting` answers this
/// and nothing else, so an update never swaps the app's code under a live tab
/// — only a page that has decided it can be reloaded asks for the swap.
fn ask_waiting_worker_in(registration: &web_sys::ServiceWorkerRegistration) {
    if let Some(waiting) = registration.waiting() {
        if let Err(error) = waiting.post_message(&JsValue::from_str("SKIP_WAITING")) {
            warn("could not hand over to the new version", &error);
        }
    }
}

/// The game saved by a previous visit, or `None`.
///
/// Every failure — no `localStorage`, no entry, unreadable text, a line written
/// by an older version — is the same answer: start a fresh board. A corrupt
/// entry must never be able to stop the app from starting.
fn stored_game(window: &Window) -> Option<Game> {
    let storage = window.local_storage().ok().flatten()?;
    let raw = storage.get_item(STORAGE_KEY).ok().flatten()?;
    Game::from_stored(&raw)
}

/// A seed for the mine placement.
///
/// Milliseconds since the epoch, which is new on every tap and needs no
/// randomness source. The cast truncates the fractional part deliberately: two
/// seeds one millisecond apart are two different boards, and a reader cannot
/// tap twice in one.
#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::cast_sign_loss
)]
fn fresh_seed() -> u64 {
    let now = js_sys::Date::now();
    if now.is_finite() && now >= 0.0 {
        now as u64
    } else {
        1
    }
}

/// The square an event is about.
fn cell_index(event: &Event) -> Option<usize> {
    let target = event.target()?.dyn_into::<Element>().ok()?;
    index_of(&target)
}

/// The square an element is in, walking up to the button that owns it.
fn index_of(element: &Element) -> Option<usize> {
    let button = element.closest("button").ok().flatten()?;
    button
        .get_attribute("data-index")
        .and_then(|text| text.parse::<usize>().ok())
}

/// Where one arrow press lands, or `None` when it would leave the board.
///
/// The board wraps in neither direction: moving off the edge of a 30-column
/// grid by one keypress is how a reader loses their place entirely.
fn neighbour(width: usize, height: usize, index: usize, key: &str) -> Option<usize> {
    let row = index / width.max(1);
    let column = index % width.max(1);
    match key {
        "ArrowLeft" if column > 0 => Some(index - 1),
        "ArrowRight" if column + 1 < width => Some(index + 1),
        "ArrowUp" if row > 0 => Some(index - width),
        "ArrowDown" if row + 1 < height => Some(index + width),
        "Home" => Some(row * width),
        "End" => Some(row * width + width - 1),
        _ => None,
    }
}

/// Attach a listener, reporting rather than panicking when it cannot.
///
/// `window`, `document` and an element are three different types with the same
/// event interface, which is why this casts to `EventTarget` rather than taking
/// one concrete type. A target that is somehow not one is a warning, not a
/// panic: the app still runs without that listener.
fn listen<T: JsCast>(target: &T, event: &str, callback: &js_sys::Function) {
    let Some(target) = target.dyn_ref::<web_sys::EventTarget>() else {
        warn("not an event target", &JsValue::from_str(event));
        return;
    };
    if let Err(error) = target.add_event_listener_with_callback(event, callback) {
        warn("could not bind an event listener", &error);
    }
}

/// A document element, or `None`.
///
/// Never panics: the shell is a separate file that someone will edit, and a
/// missing id should degrade the app, not abort it.
fn element(document: &Document, id: &str) -> Option<Element> {
    document.get_element_by_id(id)
}

/// The typed form of [`element`], so button methods are available without a cast.
fn button(document: &Document, id: &str) -> Option<HtmlButtonElement> {
    element(document, id)?.dyn_into().ok()
}

/// Replace an element's text.
fn set_text(document: &Document, id: &str, text: &str) {
    if let Some(element) = element(document, id) {
        element.set_text_content(Some(text));
    }
}

/// Report something recoverable. Never a panic, never silent.
fn warn(context: &str, error: &JsValue) {
    web_sys::console::warn_2(&JsValue::from_str(context), error);
}

/// The version this build is, on the debug channel.
///
/// Called once at startup, and it is the only reason [`crate::VERSION`] is
/// reachable code: an unused constant is dropped, and a redeploy check needs the
/// string to exist in the shipped bytes to find it. The console is where a
/// developer looks, and where nothing a reader sees is affected.
fn announce_version() {
    web_sys::console::info_1(&JsValue::from_str(&format!("version {}", crate::VERSION)));
}
