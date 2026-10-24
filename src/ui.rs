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

use std::cell::{Cell as JsCell, RefCell};
use std::rc::Rc;

use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::{spawn_local, JsFuture};
use web_sys::{
    Document, Element, Event, HtmlButtonElement, HtmlSelectElement, KeyboardEvent, Storage,
    StorageManager, Window,
};

use crate::game::{Game, Report, Status, STORAGE_KEY};
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
    /// One button per square, in board order. Rebuilt when the level changes,
    /// which is the only time the count changes.
    cells: Vec<HtmlButtonElement>,
    /// The glyph element inside each button, written to separately from the
    /// button's own text so the spoken name never picks it up.
    faces: Vec<Element>,
    /// The hidden element holding each square's spoken name.
    labels: Vec<Element>,
    /// The classes last written to each square, so a render can skip the ones
    /// that did not change: opening one square must not rewrite 480 of them.
    shown: Vec<String>,
    /// The square the arrow keys move from. Only one square is in the tab
    /// order at a time, or a keyboard reader would have to tab 480 times.
    focused: usize,
    /// Whether the next tap marks a square instead of opening it.
    flag_mode: bool,
    /// The wall clock reading at the previous tick, in milliseconds.
    last_tick: JsCell<f64>,
    /// Seconds the clock has measured but not yet handed to the game.
    ///
    /// An interval fires *approximately* every second, so the delta is as often
    /// 0.999 as it is 1.001. Discarding the fraction — or handing the game a
    /// rounded zero — stops the clock dead, which is exactly what an earlier
    /// version did: it compared the delta to one second and returned, so on this
    /// machine, whose timers run a shade early, the game showed 00:00 forever.
    /// The remainder is carried instead, so tiny early and late ticks average
    /// out to the right number of seconds.
    pending: JsCell<f64>,
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
            cells: Vec::new(),
            faces: Vec::new(),
            labels: Vec::new(),
            shown: Vec::new(),
            focused: 0,
            flag_mode: false,
            last_tick: JsCell::new(js_sys::Date::now()),
            pending: JsCell::new(0.0),
        };
        app.build_board()?;
        Ok(app)
    }

    /// Make one button per square, replacing whatever the previous board left.
    ///
    /// A board, rather than an update in place, because the number of squares
    /// depends on the level and a level change is exactly when this runs.
    fn build_board(&mut self) -> Result<(), JsValue> {
        let board =
            element(&self.document, "board").ok_or_else(|| JsValue::from_str("no #board"))?;
        board.set_inner_html("");
        let level = self.game.level();
        board.set_attribute(
            "style",
            &format!("--cols:{};--rows:{}", level.width(), level.height()),
        )?;

        self.cells.clear();
        self.faces.clear();
        self.labels.clear();
        self.shown.clear();
        self.focused = 0;

        for index in 0..level.cells() {
            let button = self
                .document
                .create_element("button")?
                .dyn_into::<HtmlButtonElement>()?;
            button.set_attribute("type", "button")?;
            button.set_attribute("data-index", &index.to_string())?;

            // The glyph is hidden from the accessible name and the spoken name
            // carries the position instead: a button whose name is "3" says
            // nothing about which of 480 squares it is.
            let face = self.document.create_element("span")?;
            face.set_class_name("face");
            face.set_attribute("aria-hidden", "true")?;
            let label = self.document.create_element("span")?;
            label.set_class_name("visually-hidden");

            button.append_child(&face)?;
            button.append_child(&label)?;
            board.append_child(&button)?;

            self.cells.push(button);
            self.faces.push(face);
            self.labels.push(label);
            self.shown.push(String::new());
        }
        Ok(())
    }

    /// Paint everything.
    fn paint(&mut self) {
        self.paint_board();
        self.paint_facts();
        self.paint_tools();
    }

    /// Paint every square: its classes, its glyph, its spoken name and whether
    /// it is the one in the tab order.
    fn paint_board(&mut self) {
        let cells = self.game.cells();
        if cells.len() != self.cells.len() {
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
            let button = &self.cells[index];
            let classes = cell.classes();
            if self.shown[index] != classes {
                button.set_class_name(&classes);
                self.shown[index] = classes;
            }
            button.set_tab_index(if index == self.focused { 0 } else { -1 });

            let face = cell.face();
            if element_text(&self.faces[index]) != face {
                self.faces[index].set_text_content(Some(&face));
            }
            let label = cell.label();
            if element_text(&self.labels[index]) != label {
                self.labels[index].set_text_content(Some(&label));
            }
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

    /// Open the square at `index`.
    fn reveal(&mut self, index: usize) {
        self.focused = index;
        let report = self.game.reveal(index);
        self.after(report);
    }

    /// Mark the square at `index`, or unmark it.
    fn flag(&mut self, index: usize) {
        self.focused = index;
        let report = self.game.flag(index);
        self.after(report);
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

    /// Flip between opening squares and marking them.
    fn toggle_flag_mode(&mut self) {
        self.flag_mode = !self.flag_mode;
        self.paint_tools();
    }

    /// Move the tab stop and the keyboard focus to `index`.
    fn focus(&mut self, index: usize) {
        if index >= self.cells.len() {
            return;
        }
        self.focused = index;
        for (position, cell) in self.cells.iter().enumerate() {
            cell.set_tab_index(if position == index { 0 } else { -1 });
        }
        let _ = self.cells[index].focus();
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
        if app.flag_mode {
            app.flag(index);
        } else {
            app.reveal(index);
        }
    });
    listen(&board, "click", click.as_ref().unchecked_ref());
    click.forget();

    // A right click marks a square: the desktop habit, and it costs one
    // listener. The phone's equivalent is the Flag button above; a long press
    // would be a second path to the same state that nobody discovers.
    let shared = Rc::clone(app);
    let mark = Closure::<dyn FnMut(Event)>::new(move |event: Event| {
        let Some(index) = cell_index(&event) else {
            return;
        };
        event.prevent_default();
        shared.borrow_mut().flag(index);
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
    let reloaded = Rc::new(JsCell::new(false));
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

/// The text of an element, or `""`.
fn element_text(element: &Element) -> String {
    element.text_content().unwrap_or_default()
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
