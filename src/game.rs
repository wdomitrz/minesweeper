//! One game: what is covered, what is flagged, and how it ended.
//!
//! The rules live here, and every word the page shows about them. That is
//! deliberate: `ui.rs` may write a class name onto a button, but it never
//! decides what a square *means*, and it never spells a sentence. A readout or
//! a report is built here, from the state, so the clock, the counter and the
//! status line cannot disagree with the board.
//!
//! The game is also the thing that is saved. Its stored form is a handful of
//! numbers and a letter per square, which is why the seed and the first click
//! are kept: the minefield is rebuilt from them rather than stored, and a
//! corrupt or older entry is simply not a game.

use crate::minefield::{around, Level, Minefield};

/// The `localStorage` key this game is saved under.
///
/// Versioned in the name: a future format can ship beside this one instead of
/// guessing whether an old entry parses.
pub const STORAGE_KEY: &str = "minesweeper.game.v1";

/// The longest a single clock tick may add, in seconds.
///
/// A tab that was hidden for an hour reports one large delta when it comes
/// back, and counting it is right — the game was running. Counting a bogus
/// clock jump (a device waking with a wrong date) is not, so one tick is
/// capped here and the total below.
const MAX_TICK_SECONDS: u32 = 3_600;

/// The longest a game's clock may run: 99:59:59.
///
/// A number nobody will reach, chosen so the clock's own formatting cannot
/// overflow whatever it is drawn into.
const MAX_ELAPSED: u32 = 359_999;

/// How a square is covered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cover {
    /// Nobody has opened it and nobody has marked it.
    Hidden,
    /// Marked as a suspected mine.
    Flagged,
    /// Opened: it shows its number, or the mine that ended the game.
    Revealed,
}

/// How the game stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// No square has been opened, so the mines are not placed yet.
    Ready,
    /// The clock is running and squares can be opened.
    Playing,
    /// Every square that is not a mine has been opened.
    Won,
    /// A mine was opened.
    Lost,
}

impl Status {
    /// The stable name this status is stored under.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Playing => "playing",
            Self::Won => "won",
            Self::Lost => "lost",
        }
    }

    /// The status a stored slug names, or `None`.
    pub fn from_slug(slug: &str) -> Option<Self> {
        [Self::Ready, Self::Playing, Self::Won, Self::Lost]
            .into_iter()
            .find(|status| status.slug() == slug)
    }
}

/// One game of minesweeper.
#[derive(Clone, Debug)]
pub struct Game {
    level: Level,
    /// The seed the mines are placed with, kept so a saved game can be rebuilt.
    seed: u64,
    /// The mines, once the first square has been opened.
    field: Option<Minefield>,
    /// The first square opened — the safe one — which the field depends on.
    first: Option<usize>,
    cover: Vec<Cover>,
    flags: usize,
    status: Status,
    elapsed: u32,
    /// The square whose mine ended the game, if one did.
    exploded: Option<usize>,
}

impl Game {
    /// A fresh game on `level`, with `seed` deciding where the mines go.
    ///
    /// `seed` is a parameter rather than drawn here so that the whole rule set
    /// can be tested without a clock; the page passes the current time.
    pub fn new(level: Level, seed: u64) -> Self {
        Self {
            level,
            seed,
            field: None,
            first: None,
            cover: vec![Cover::Hidden; level.cells()],
            flags: 0,
            status: Status::Ready,
            elapsed: 0,
            exploded: None,
        }
    }

    /// The level this game is played on.
    pub fn level(&self) -> Level {
        self.level
    }

    /// How the game stands.
    pub fn status(&self) -> Status {
        self.status
    }

    /// Whether the game is over, won or lost.
    pub fn is_finished(&self) -> bool {
        matches!(self.status, Status::Won | Status::Lost)
    }

    /// Whether the board has been opened at all — the clock is only worth
    /// showing once it has.
    pub fn has_started(&self) -> bool {
        self.status != Status::Ready
    }

    /// Seconds played.
    pub fn elapsed(&self) -> u32 {
        self.elapsed
    }

    /// Mines not yet flagged.
    ///
    /// Never negative: [`Self::flag`] refuses a flag once every mine is
    /// accounted for, rather than letting the counter run past zero.
    pub fn mines_remaining(&self) -> usize {
        self.level.mines().saturating_sub(self.flags)
    }

    /// How many squares are flagged.
    pub fn flags(&self) -> usize {
        self.flags
    }

    /// Open the square at `index`.
    ///
    /// The first call places the mines with that square and its neighbours
    /// clear, so a first click can never lose. Opening a mine ends the game;
    /// opening a square with no neighbouring mines opens everything reachable
    /// from it, which is what makes the board playable.
    pub fn reveal(&mut self, index: usize) -> Report {
        if index >= self.cover.len() || self.is_finished() {
            return Report::Nothing;
        }
        match self.cover[index] {
            Cover::Flagged => return Report::AlreadyFlagged,
            Cover::Revealed => return Report::AlreadyOpen,
            Cover::Hidden => {}
        }

        if self.field.is_none() {
            self.field = Some(Minefield::new(self.level, self.seed, index));
            self.first = Some(index);
            self.status = Status::Playing;
        }

        let opened_a_mine = self
            .field
            .as_ref()
            .is_some_and(|field| field.is_mine(index));
        if opened_a_mine {
            self.cover[index] = Cover::Revealed;
            self.exploded = Some(index);
            self.status = Status::Lost;
            return Report::Lost;
        }

        let opened = self.open_area(index);
        if self.is_won() {
            self.status = Status::Won;
            return Report::Won(self.elapsed);
        }
        Report::Opened(opened)
    }

    /// Open a square, or chord on one that is already open and numbered.
    ///
    /// This is what a click means, and the whole reason it exists as one method:
    /// whether a square opens or chords is a rule, so it is decided here rather
    /// than by the page guessing from classes it just wrote. On a covered square
    /// this is exactly [`Self::reveal`]; on an open number it is
    /// [`Self::chord`], because a number that has all its flags beside it is not
    /// a square to open, it is a button for the squares around it.
    pub fn open(&mut self, index: usize) -> Report {
        if self.is_chordable(index) {
            return self.chord(index);
        }
        self.reveal(index)
    }

    /// Whether a click on `index` would chord rather than open.
    ///
    /// The page uses this to draw the squares a chord would take, so the reader
    /// can see what they are about to do before they do it — and a wrong chord
    /// is a game.
    pub fn is_chordable(&self, index: usize) -> bool {
        if index >= self.cover.len() || self.is_finished() {
            return false;
        }
        if self.cover[index] != Cover::Revealed {
            return false;
        }
        self.field
            .as_ref()
            .is_some_and(|field| !field.is_mine(index) && field.count(index) > 0)
    }

    /// Open every covered square beside an open number, when the flags beside it
    /// already account for it.
    ///
    /// This is the move every minesweeper has under some name, and the only one
    /// that can *lose* a game the player was not touching: the flags are a claim
    /// about the board, and a chord acts on the claim. Put two flags on the
    /// wrong squares beside a `2` and the chord opens the mine they were hiding.
    /// That is the point — a board that only ever rewarded correct flags would
    /// make the flag counter a score rather than a tool.
    ///
    /// The mine check runs over the whole neighbourhood before anything opens, so
    /// a chord that loses does not also leave a half-opened ring behind.
    pub fn chord(&mut self, index: usize) -> Report {
        if !self.is_chordable(index) {
            return Report::Nothing;
        }
        let Some(field) = self.field.as_ref() else {
            return Report::Nothing;
        };
        let (width, height) = (field.width(), field.height());
        let count = field.count(index);
        let neighbours: Vec<usize> = around(width, height, index).collect();
        let flagged = neighbours
            .iter()
            .filter(|neighbour| self.cover[**neighbour] == Cover::Flagged)
            .count();
        let wanted = usize::from(count);
        if flagged != wanted {
            // More flags than the number needs is as wrong as fewer, and opening
            // on either would be acting on a claim the player has not made.
            return Report::FlagsDoNotMatch {
                count,
                flagged,
                wanted,
            };
        }

        let mine = neighbours
            .iter()
            .find(|neighbour| {
                self.cover[**neighbour] == Cover::Hidden && field.is_mine(**neighbour)
            })
            .copied();
        if let Some(mine) = mine {
            self.cover[mine] = Cover::Revealed;
            self.exploded = Some(mine);
            self.status = Status::Lost;
            return Report::Lost;
        }

        let hidden: Vec<usize> = neighbours
            .into_iter()
            .filter(|neighbour| self.cover[*neighbour] == Cover::Hidden)
            .collect();
        if hidden.is_empty() {
            return Report::Nothing;
        }
        let opened = hidden
            .into_iter()
            .map(|neighbour| self.open_area(neighbour))
            .sum();
        if self.is_won() {
            self.status = Status::Won;
            return Report::Won(self.elapsed);
        }
        Report::Opened(opened)
    }

    /// Mark the square at `index` as a suspected mine, or unmark it.
    ///
    /// Flagging cannot start the game: the clock waits for the first square
    /// actually opened, because thinking about the board is not playing it.
    pub fn flag(&mut self, index: usize) -> Report {
        if index >= self.cover.len() || self.is_finished() {
            return Report::Nothing;
        }
        match self.cover[index] {
            Cover::Revealed => Report::AlreadyOpen,
            Cover::Flagged => {
                self.cover[index] = Cover::Hidden;
                self.flags = self.flags.saturating_sub(1);
                Report::Nothing
            }
            Cover::Hidden => {
                if self.flags >= self.level.mines() {
                    return Report::AllFlagsPlaced(self.level.mines());
                }
                self.cover[index] = Cover::Flagged;
                self.flags += 1;
                Report::Nothing
            }
        }
    }

    /// Advance the clock by `seconds`, while the game is being played.
    pub fn tick(&mut self, seconds: u32) {
        if self.status != Status::Playing {
            return;
        }
        self.elapsed = self
            .elapsed
            .saturating_add(seconds.min(MAX_TICK_SECONDS))
            .min(MAX_ELAPSED);
    }

    /// Every square, as it should be drawn and described.
    pub fn cells(&self) -> Vec<Cell> {
        let width = self.level.width();
        let lost = self.status == Status::Lost;
        (0..self.cover.len())
            .map(|index| {
                let cover = self.cover[index];
                let field = self.field.as_ref();
                // Mines are only shown once the game is lost, and a square the
                // player got right keeps its flag: taking it away would punish
                // the one thing they did correctly.
                let mine = field.is_some_and(|field| lost && field.is_mine(index))
                    && cover != Cover::Flagged;
                let count = match field {
                    Some(field) if cover == Cover::Revealed && !field.is_mine(index) => {
                        field.count(index)
                    }
                    _ => 0,
                };
                Cell {
                    row: index / width + 1,
                    column: index % width + 1,
                    cover,
                    count,
                    mine,
                    exploded: self.exploded == Some(index),
                }
            })
            .collect()
    }

    /// The clock, the counter and the outcome, in the words the page shows.
    pub fn readout(&self) -> Readout {
        Readout {
            clock: clock_words(self.elapsed),
            remaining: self.mines_remaining().to_string(),
            summary: outcome_words(self.status, self.elapsed),
        }
    }

    /// The game in its stored form.
    pub fn to_stored(&self) -> String {
        let cover: String = self
            .cover
            .iter()
            .map(|cover| match cover {
                Cover::Hidden => '.',
                Cover::Flagged => 'F',
                Cover::Revealed => 'R',
            })
            .collect();
        format!(
            "v1|{}|{}|{}|{}|{}|{}|{}",
            self.level.slug(),
            self.seed,
            self.first
                .map_or_else(|| "-".to_owned(), |index| index.to_string()),
            self.status.slug(),
            self.elapsed,
            cover,
            self.exploded
                .map_or_else(|| "-".to_owned(), |index| index.to_string()),
        )
    }

    /// A game from [`Self::to_stored`], or `None` for anything that is not one.
    ///
    /// Every field is checked, and every check that fails returns `None`. A
    /// saved game is input from outside this program: it can be truncated, it
    /// can come from a version that wrote a different format, and a player can
    /// edit it. None of that may stop the app from starting.
    pub fn from_stored(raw: &str) -> Option<Self> {
        let mut parts = raw.split('|');
        if parts.next()? != "v1" {
            return None;
        }
        let level = Level::from_slug(parts.next()?)?;
        let seed: u64 = parts.next()?.parse().ok()?;
        let first = stored_index(parts.next()?, level.cells())?;
        let status = Status::from_slug(parts.next()?)?;
        let elapsed: u32 = parts.next()?.parse().ok()?;
        let cover_text = parts.next()?;
        let exploded = stored_index(parts.next()?, level.cells())?;
        if parts.next().is_some() {
            return None;
        }

        // One character per square, and only the three that mean something.
        if cover_text.chars().count() != level.cells() {
            return None;
        }
        let mut cover = Vec::with_capacity(level.cells());
        for letter in cover_text.chars() {
            cover.push(match letter {
                '.' => Cover::Hidden,
                'F' => Cover::Flagged,
                'R' => Cover::Revealed,
                _ => return None,
            });
        }

        // The minefield is rebuilt from the seed and the first click, so a game
        // that has been played must have one, and a game that has not must not.
        if status == Status::Ready {
            if first.is_some() || cover.iter().any(|cover| *cover != Cover::Hidden) {
                return None;
            }
        } else if first.is_none() {
            return None;
        }
        // A clock that ran in a tab this build did not see can be long; it is
        // clamped rather than refused, because the game itself is still sound.
        let elapsed = elapsed.min(MAX_ELAPSED);
        let flags = cover
            .iter()
            .filter(|cover| **cover == Cover::Flagged)
            .count();
        if flags > level.mines() {
            return None;
        }
        let field = first.map(|first| Minefield::new(level, seed, first));

        Some(Self {
            level,
            seed,
            field,
            first,
            cover,
            flags,
            status,
            elapsed,
            exploded,
        })
    }

    /// Open `start` and, if it has no neighbouring mines, everything reachable
    /// through squares that do not either.
    ///
    /// A stack rather than recursion: a clear corner of an expert board is 480
    /// squares, and the recursive version of this is a crash waiting for one
    /// unlucky board.
    fn open_area(&mut self, start: usize) -> usize {
        let Some(field) = self.field.as_ref() else {
            return 0;
        };
        let (width, height) = (field.width(), field.height());
        let mut opened = 0;
        let mut stack = vec![start];
        while let Some(index) = stack.pop() {
            if self.cover[index] != Cover::Hidden {
                continue;
            }
            self.cover[index] = Cover::Revealed;
            opened += 1;
            if field.count(index) != 0 {
                continue;
            }
            for neighbour in around(width, height, index) {
                if self.cover[neighbour] == Cover::Hidden && !field.is_mine(neighbour) {
                    stack.push(neighbour);
                }
            }
        }
        opened
    }

    /// Whether every square that is not a mine has been opened.
    fn is_won(&self) -> bool {
        let Some(field) = self.field.as_ref() else {
            return false;
        };
        (0..self.cover.len())
            .filter(|index| !field.is_mine(*index))
            .all(|index| self.cover[index] == Cover::Revealed)
    }
}

/// One square, as the page draws it and as a screen reader describes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    /// 1-based, for the spoken name.
    pub row: usize,
    /// 1-based, for the spoken name.
    pub column: usize,
    pub cover: Cover,
    /// Neighbouring mines, meaningful only on a revealed square that is not one.
    pub count: u8,
    /// A mine the page should show, which is only true once the game is lost.
    pub mine: bool,
    /// The mine that ended the game.
    pub exploded: bool,
}

impl Cell {
    /// The class tokens the page puts on this square's button.
    ///
    /// Built here rather than in `ui.rs` so that what a square looks like is
    /// decided beside what it means.
    pub fn classes(&self) -> String {
        let mut classes = String::from("cell ");
        classes.push_str(match self.cover {
            Cover::Hidden => "hidden",
            Cover::Flagged => "flagged",
            Cover::Revealed => "revealed",
        });
        if self.cover == Cover::Revealed && self.count > 0 && !self.mine {
            // The eight number colours are separate tokens; a square with one
            // mine beside it says `n1`, not `count-1`.
            classes.push(' ');
            classes.push_str(&format!("n{}", self.count));
        }
        if self.mine {
            classes.push_str(" mine");
        }
        if self.exploded {
            classes.push_str(" exploded");
        }
        classes
    }

    /// The glyph or number drawn in the square.
    ///
    /// A flag wins over a mine, because a mine the player marked correctly is
    /// still their flag. `●` is a mine; `⚑` is a flag; a clear square is empty.
    pub fn face(&self) -> String {
        if self.mine {
            return "●".to_owned();
        }
        match self.cover {
            Cover::Flagged => "⚑".to_owned(),
            Cover::Revealed if self.count > 0 => self.count.to_string(),
            Cover::Revealed | Cover::Hidden => String::new(),
        }
    }

    /// What a screen reader says for this square.
    ///
    /// The position is spoken because the visible glyph is not: "7" alone says
    /// nothing about which of 480 squares it is.
    pub fn label(&self) -> String {
        let place = format!("Row {}, column {}", self.row, self.column);
        if self.exploded {
            return format!("{place}, the mine you opened");
        }
        if self.mine {
            return format!("{place}, a mine");
        }
        match self.cover {
            Cover::Flagged => format!("{place}, flagged"),
            Cover::Revealed => match self.count {
                0 => format!("{place}, clear"),
                1 => format!("{place}, 1 mine around"),
                count => format!("{place}, {count} mines around"),
            },
            Cover::Hidden => format!("{place}, covered"),
        }
    }
}

/// The clock, the counter and the outcome: what the header shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Readout {
    /// `MM:SS`, or `H:MM:SS` past an hour.
    pub clock: String,
    /// Mines not yet flagged.
    pub remaining: String,
    /// What happened, once something has: empty while the game is on.
    pub summary: String,
}

/// What just happened, in the words the status line shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Report {
    /// Nothing worth interrupting the reader for.
    Nothing,
    /// Squares opened by one click. One square says nothing the board does not.
    Opened(usize),
    /// The square is already open.
    AlreadyOpen,
    /// The square is flagged, and a flag has to be removed first.
    AlreadyFlagged,
    /// Every mine already has a flag.
    AllFlagsPlaced(usize),
    /// A chord was asked for on a number whose flags do not account for it.
    FlagsDoNotMatch {
        /// The number in the square.
        count: u8,
        /// Flags actually beside it.
        flagged: usize,
        /// Flags the number says should be.
        wanted: usize,
    },
    /// The game was won, in this many seconds.
    Won(u32),
    /// A mine was opened.
    Lost,
}

impl Report {
    /// The sentence the status line shows, or `""` for nothing.
    pub fn words(&self) -> String {
        match self {
            Self::Nothing | Self::Opened(0 | 1) => String::new(),
            Self::Opened(count) => format!("Opened {count} squares."),
            // Also silent, and for the same reason as a chord that cannot run:
            // tapping an open square is how a chord is attempted, so most of
            // these arrive in the middle of playing and say nothing the reader
            // did not just do. A flagged square is visibly flagged.
            //
            // (Both were removed on 2026-10-24 after the sentences showed up as
            // narration. The variants stay: they are the rule's answers, and the
            // tests assert which one came back.)
            Self::AlreadyOpen | Self::AlreadyFlagged => String::new(),
            Self::AllFlagsPlaced(mines) => {
                format!("All {mines} flags are placed. Remove one to mark another.")
            }
            // A chord that cannot run is not news. The reader can see how many
            // flags are beside the number — they put them there, one at a time,
            // seconds ago — so a sentence counting them back is a status line
            // narrating what the board already shows. This was tried and read as
            // noise ("That 2 has only 1 of the 2 flags it needs"), which is why
            // the words are empty here: the pattern is still returned, because
            // the *rule* refused the move and the tests say so.
            Self::FlagsDoNotMatch { .. } => String::new(),
            Self::Won(seconds) => outcome_words(Status::Won, *seconds),
            Self::Lost => outcome_words(Status::Lost, 0),
        }
    }
}

/// What the game says about how it ended, or `""` while it has not.
///
/// One function, because both the status line and the restored readout say this
/// and they must not drift into two slightly different sentences.
fn outcome_words(status: Status, elapsed: u32) -> String {
    match status {
        Status::Won => format!("Cleared in {}.", duration_words(elapsed)),
        Status::Lost => "You opened a mine.".to_owned(),
        Status::Ready | Status::Playing => String::new(),
    }
}

/// `MM:SS`, or `H:MM:SS` once there is an hour to show.
fn clock_words(seconds: u32) -> String {
    let hours = seconds / 3_600;
    let minutes = seconds % 3_600 / 60;
    let seconds = seconds % 60;
    if hours > 0 {
        format!("{hours}:{minutes:02}:{seconds:02}")
    } else {
        format!("{minutes:02}:{seconds:02}")
    }
}

/// A duration as prose: "42 seconds", "1 minute 5 seconds".
fn duration_words(seconds: u32) -> String {
    let minutes = seconds / 60;
    let seconds = seconds % 60;
    let minutes_words = match minutes {
        0 => String::new(),
        1 => "1 minute".to_owned(),
        minutes => format!("{minutes} minutes"),
    };
    let seconds_words = match seconds {
        0 => String::new(),
        1 => "1 second".to_owned(),
        seconds => format!("{seconds} seconds"),
    };
    match (minutes_words.is_empty(), seconds_words.is_empty()) {
        (true, _) => seconds_words,
        (false, true) => minutes_words,
        (false, false) => format!("{minutes_words} {seconds_words}"),
    }
}

/// A stored square index, where `-` means there is none.
///
/// Returns `None` for a missing index that must exist, a number that does not
/// parse, or a number outside the board — the caller treats all three the same
/// way, as a game that cannot be restored.
fn stored_index(text: &str, cells: usize) -> Option<Option<usize>> {
    if text == "-" {
        return Some(None);
    }
    let index: usize = text.parse().ok()?;
    if index >= cells {
        return None;
    }
    Some(Some(index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::minefield::Level;

    fn started(level: Level, seed: u64) -> Game {
        let mut game = Game::new(level, seed);
        game.reveal(0);
        game
    }

    fn mine_of(game: &Game) -> usize {
        let field = game.field.as_ref().expect("the field exists once played");
        (0..field.cells())
            .find(|index| field.is_mine(*index))
            .expect("a board with mines has a mine")
    }

    #[test]
    fn a_new_game_is_ready_and_unstarted() {
        for level in Level::ALL {
            let game = Game::new(level, 1);
            assert_eq!(game.status(), Status::Ready);
            assert!(!game.has_started());
            assert!(!game.is_finished());
            assert_eq!(game.elapsed(), 0);
            assert_eq!(game.flags(), 0);
            assert_eq!(game.mines_remaining(), level.mines());
            assert_eq!(game.readout().clock, "00:00");
            assert_eq!(game.readout().remaining, level.mines().to_string());
            assert!(game.readout().summary.is_empty());
            assert!(game.cells().iter().all(|cell| cell.cover == Cover::Hidden));
        }
    }

    #[test]
    fn the_first_square_never_loses_whatever_the_seed() {
        for level in Level::ALL {
            let board = level.cells();
            for step in 0..20 {
                let first = (step * 7_919) % board;
                let mut game = Game::new(level, 1_000 + step as u64);
                let report = game.reveal(first);
                assert_ne!(report, Report::Lost, "{} first {first}", level.label());
                assert_eq!(game.status(), Status::Playing);
                assert!(game.has_started());
            }
        }
    }

    #[test]
    fn the_first_square_opens_more_than_itself() {
        // The first square and its neighbours are clear, so it counts zero and
        // the flood opens at least its whole neighbourhood.
        for level in Level::ALL {
            let mut game = Game::new(level, 20_260_102);
            let report = game.reveal(level.cells() / 2 + 1);
            match report {
                Report::Opened(opened) => assert!(opened > 1, "{}", level.label()),
                other => panic!("expected a flood, got {other:?}"),
            }
        }
    }

    #[test]
    fn the_first_click_is_the_one_the_field_was_built_around() {
        let mut game = Game::new(Level::Beginner, 55);
        game.reveal(40);
        assert_eq!(game.first, Some(40));
        assert!(!game.field.as_ref().expect("field").is_mine(40));
    }

    #[test]
    fn opening_a_mine_ends_the_game_and_marks_it() {
        let mut game = started(Level::Beginner, 7);
        let mine = mine_of(&game);
        let report = game.reveal(mine);
        assert_eq!(report, Report::Lost);
        assert_eq!(game.status(), Status::Lost);
        assert!(game.is_finished());
        assert_eq!(game.exploded, Some(mine));

        let cell = &game.cells()[mine];
        assert!(cell.mine);
        assert!(cell.exploded);
        assert!(cell.classes().contains("mine"));
        assert!(cell.classes().contains("exploded"));
        assert_eq!(cell.face(), "●");
        assert_eq!(
            cell.label(),
            format!(
                "Row {}, column {}, the mine you opened",
                mine / 9 + 1,
                mine % 9 + 1
            )
        );
    }

    #[test]
    fn losing_shows_every_mine_but_keeps_a_correct_flag() {
        let mut game = started(Level::Beginner, 11);
        let mine = mine_of(&game);
        game.flag(mine);
        let other = (0..81)
            .find(|index| index != &mine && game.field.as_ref().expect("field").is_mine(*index))
            .expect("beginner has ten mines");
        game.reveal(other);
        assert_eq!(game.status(), Status::Lost);
        let cells = game.cells();
        let flagged = &cells[mine];
        assert_eq!(flagged.cover, Cover::Flagged);
        assert!(
            !flagged.mine,
            "a flagged mine keeps its flag, not a mine glyph"
        );
        assert_eq!(flagged.face(), "⚑");
        assert!(cells[other].mine);
    }

    #[test]
    fn winning_needs_every_safe_square_open() {
        let mut game = started(Level::Beginner, 3);
        game.tick(42);
        // Open every safe square until the board says it is done. Mines are
        // skipped — opening one is how a game is lost, which is a different
        // test. A flood can open the last few at once, so the loop stops on the
        // state rather than counting reveals: this asserts the rule, not one
        // particular board's shape.
        let mines: Vec<usize> = {
            let field = game.field.as_ref().expect("field");
            (0..field.cells())
                .filter(|index| field.is_mine(*index))
                .collect()
        };
        let mut last = Report::Nothing;
        for index in 0..game.cells().len() {
            if game.is_finished() {
                break;
            }
            if mines.contains(&index) {
                continue;
            }
            last = game.reveal(index);
        }
        assert_eq!(game.status(), Status::Won);
        assert!(game.is_finished());
        assert_eq!(last.words(), "Cleared in 42 seconds.");
        assert_eq!(game.readout().summary, "Cleared in 42 seconds.");
    }

    /// A board with a known, tiny neighbourhood to chord on: a real beginner
    /// game, with the index of a numbered square and its neighbours read off the
    /// minefield rather than assumed.
    fn numbered_game(seed: u64) -> (Game, usize, Vec<usize>) {
        let game = started(Level::Beginner, seed);
        let field = game.field.as_ref().expect("field");
        let width = field.width();
        let index = (0..game.cells().len())
            .find(|index| {
                game.cover[*index] == Cover::Revealed
                    && !field.is_mine(*index)
                    && field.count(*index) > 0
                    && around(width, field.height(), *index)
                        .any(|neighbour| game.cover[neighbour] == Cover::Hidden)
            })
            .expect("a first click always leaves a numbered square with a covered neighbour");
        let neighbours = around(width, field.height(), index).collect();
        (game, index, neighbours)
    }

    #[test]
    fn clicking_an_open_number_opens_the_squares_it_says_are_clear() {
        let (mut game, index, neighbours) = numbered_game(4_242);
        let field = game.field.as_ref().expect("field");
        let count = field.count(index);
        // Flag exactly the mines beside it, which is what a player who has read
        // the number would do.
        let mines: Vec<usize> = neighbours
            .iter()
            .copied()
            .filter(|neighbour| field.is_mine(*neighbour))
            .collect();
        assert_eq!(mines.len(), usize::from(count));
        for mine in &mines {
            if game.cover[*mine] == Cover::Hidden {
                game.flag(*mine);
            }
        }
        let report = game.chord(index);
        assert!(
            matches!(report, Report::Opened(opened) if opened > 0),
            "a chord with the right flags opens the rest: {report:?}"
        );
        // ...and the squares the number said were clear are now open.
        let field = game.field.as_ref().expect("field");
        for neighbour in &neighbours {
            if field.is_mine(*neighbour) {
                assert_eq!(game.cover[*neighbour], Cover::Flagged);
            } else {
                assert_ne!(
                    game.cover[*neighbour],
                    Cover::Hidden,
                    "a square the number said was clear stayed covered"
                );
            }
        }
    }

    #[test]
    fn a_chord_with_too_few_flags_says_so_and_opens_nothing() {
        let (mut game, index, _) = numbered_game(777);
        let before = game.cover.clone();
        let report = game.chord(index);
        assert!(
            matches!(report, Report::FlagsDoNotMatch { flagged: 0, .. }),
            "{report:?}"
        );
        assert_eq!(game.cover, before, "nothing may open without the flags");
        assert!(
            report.words().is_empty(),
            "a chord that cannot run must say nothing: it narrates what the board \
             already shows. Said {:?}",
            report.words()
        );
    }

    #[test]
    fn a_chord_with_too_many_flags_opens_nothing() {
        // A number with more covered neighbours than it has mines beside it, so
        // there is room to plant one flag too many. One seed per attempt, and the
        // first board that offers such a square is the one the test uses: which
        // board it is does not matter, only that over-flagging is refused.
        let (mut game, index, neighbours) = (0..200u64)
            .find_map(|seed| {
                let (game, index, neighbours) = numbered_game(seed * 977 + 5);
                let count = usize::from(game.field.as_ref().expect("field").count(index));
                let hidden = neighbours
                    .iter()
                    .filter(|neighbour| game.cover[**neighbour] == Cover::Hidden)
                    .count();
                (hidden > count).then_some((game, index, neighbours))
            })
            .expect("some beginner board offers a number with a covered neighbour to spare");
        let count = usize::from(game.field.as_ref().expect("field").count(index));
        let mut flagged = 0;
        for neighbour in &neighbours {
            if flagged > count {
                break;
            }
            if game.cover[*neighbour] == Cover::Hidden {
                game.flag(*neighbour);
                flagged += 1;
            }
        }
        assert_eq!(flagged, count + 1);
        let before = game.cover.clone();
        let report = game.chord(index);
        assert!(
            matches!(report, Report::FlagsDoNotMatch { count: c, flagged: f, .. } if usize::from(c) == count && f == flagged),
            "over-flagging must not be acted on either: {report:?}"
        );
        assert_eq!(game.cover, before);
        assert!(report.words().is_empty());
    }

    #[test]
    fn a_chord_on_wrong_flags_loses_the_game() {
        // The case worth guarding: the flags are a claim, and a chord acts on it.
        // A board where this could never lose would make the flag counter a score
        // rather than a tool.
        let (mut game, index, neighbours) = numbered_game(20_261_024);
        let field = game.field.as_ref().expect("field");
        let count = usize::from(field.count(index));
        let safe: Vec<usize> = neighbours
            .iter()
            .copied()
            .filter(|neighbour| {
                !field.is_mine(*neighbour) && game.cover[*neighbour] == Cover::Hidden
            })
            .collect();
        assert!(
            safe.len() >= count,
            "this seed cannot lie about this number's flags; try another"
        );
        // Flag the wrong squares: as many flags as the number wants, none of them
        // on a mine.
        for square in safe.iter().take(count) {
            game.flag(*square);
        }
        let report = game.chord(index);
        assert_eq!(report, Report::Lost, "a wrong chord must be able to lose");
        assert_eq!(game.status(), Status::Lost);
        let exploded = game.exploded.expect("the mine that ended it");
        assert!(!safe.contains(&exploded) || game.field.as_ref().expect("field").is_mine(exploded));
        assert!(game.field.as_ref().expect("field").is_mine(exploded));
    }

    #[test]
    fn a_chord_wins_the_game_when_it_opens_the_last_squares() {
        let (mut game, index, neighbours) = numbered_game(9_999);
        // Open everything except the mines and this neighbourhood first, so the
        // chord is the last move.
        let mines: Vec<usize> = {
            let field = game.field.as_ref().expect("field");
            (0..field.cells()).filter(|i| field.is_mine(*i)).collect()
        };
        let elsewhere: Vec<usize> = (0..game.cells().len())
            .filter(|i| !mines.contains(i) && !neighbours.contains(i))
            .collect();
        for square in elsewhere {
            if game.is_finished() {
                break;
            }
            game.reveal(square);
        }
        // Flag the mines beside the number and chord it.
        let mines_here: Vec<usize> = neighbours
            .iter()
            .copied()
            .filter(|neighbour| {
                game.field
                    .as_ref()
                    .is_some_and(|field| field.is_mine(*neighbour))
            })
            .collect();
        for mine in mines_here {
            if game.cover[mine] == Cover::Hidden {
                game.flag(mine);
            }
        }
        let report = game.chord(index);
        assert_eq!(game.status(), Status::Won, "{report:?}");
        assert!(game.is_finished());
    }

    #[test]
    fn open_chords_an_open_number_and_reveals_anything_else() {
        let (game, index, _) = numbered_game(5_515);
        assert!(game.is_chordable(index));

        // A covered square is never a chord, even when it has numbers around it.
        let covered = (0..game.cells().len())
            .find(|i| game.cover[*i] == Cover::Hidden)
            .expect("a covered square");
        assert!(!game.is_chordable(covered));

        // A clear, already-open square has nothing to say about its neighbours.
        let clear = (0..game.cells().len())
            .find(|i| {
                game.cover[*i] == Cover::Revealed
                    && game
                        .field
                        .as_ref()
                        .is_some_and(|field| field.count(*i) == 0)
            })
            .expect("a first click opens clear squares");
        assert!(!game.is_chordable(clear));

        // And out-of-range indices are refused rather than panicking.
        assert!(!game.is_chordable(usize::MAX));
    }

    #[test]
    fn opening_a_covered_square_is_the_same_as_revealing_it() {
        // Same seed, same first square, so the two boards must stay identical:
        // `open` is allowed to branch on a square being open already, and
        // nothing else.
        let mut opened_through_open = Game::new(Level::Beginner, 12);
        let mut opened_through_reveal = Game::new(Level::Beginner, 12);
        let via_open = opened_through_open.open(40);
        let via_reveal = opened_through_reveal.reveal(40);
        assert_eq!(via_open, via_reveal);
        assert_eq!(
            opened_through_open.to_stored(),
            opened_through_reveal.to_stored()
        );
        assert_eq!(opened_through_open.status(), Status::Playing);
    }

    #[test]
    fn a_game_that_has_not_been_won_is_not_won() {
        let game = started(Level::Beginner, 3);
        assert!(
            !game.is_won(),
            "a fresh board has safe squares still covered"
        );
        assert_eq!(game.status(), Status::Playing);
    }

    #[test]
    fn flags_toggle_and_the_counter_follows() {
        let mut game = Game::new(Level::Beginner, 1);
        assert_eq!(game.flag(5), Report::Nothing);
        assert_eq!(game.cells()[5].cover, Cover::Flagged);
        assert_eq!(game.flags(), 1);
        assert_eq!(game.mines_remaining(), 9);

        assert_eq!(game.flag(5), Report::Nothing);
        assert_eq!(game.cells()[5].cover, Cover::Hidden);
        assert_eq!(game.flags(), 0);
        assert_eq!(game.mines_remaining(), 10);
    }

    #[test]
    fn flagging_does_not_start_the_clock() {
        let mut game = Game::new(Level::Beginner, 1);
        game.flag(0);
        assert_eq!(game.status(), Status::Ready);
        game.tick(10);
        assert_eq!(game.elapsed(), 0);
    }

    #[test]
    fn the_flag_count_is_the_mine_count_and_no_more() {
        let mut game = Game::new(Level::Beginner, 1);
        for index in 0..10 {
            assert_eq!(game.flag(index), Report::Nothing);
        }
        assert_eq!(game.flags(), 10);
        assert_eq!(game.mines_remaining(), 0);
        assert_eq!(
            game.flag(10),
            Report::AllFlagsPlaced(10),
            "an eleventh flag is refused, not counted"
        );
        assert_eq!(game.flags(), 10);
        assert_eq!(game.cells()[10].cover, Cover::Hidden);
        assert_eq!(
            game.flag(10).words(),
            "All 10 flags are placed. Remove one to mark another."
        );
    }

    #[test]
    fn an_open_square_cannot_be_flagged_and_a_flagged_one_cannot_be_opened() {
        let mut game = Game::new(Level::Beginner, 4);
        game.flag(0);
        assert_eq!(game.reveal(0), Report::AlreadyFlagged);
        assert_eq!(game.reveal(0).words(), "");

        let mut game = Game::new(Level::Beginner, 4);
        game.reveal(0);
        assert_eq!(game.flag(0), Report::AlreadyOpen);
        assert_eq!(game.reveal(0), Report::AlreadyOpen);
        assert_eq!(game.reveal(0).words(), "");
    }

    #[test]
    fn nothing_can_be_done_once_the_game_is_over() {
        let mut game = started(Level::Beginner, 7);
        let mine = mine_of(&game);
        game.reveal(mine);
        let cover_before = game.cover.clone();
        let flags_before = game.flags();
        assert_eq!(game.reveal(mine), Report::Nothing);
        assert_eq!(game.flag(5), Report::Nothing);
        assert_eq!(game.cover, cover_before);
        assert_eq!(game.flags(), flags_before);
    }

    #[test]
    fn an_index_outside_the_board_is_ignored() {
        let mut game = Game::new(Level::Beginner, 1);
        assert_eq!(game.reveal(81), Report::Nothing);
        assert_eq!(game.flag(81), Report::Nothing);
        assert_eq!(game.status(), Status::Ready);
        assert_eq!(game.flags(), 0);
    }

    #[test]
    fn the_clock_only_runs_while_playing() {
        let mut game = Game::new(Level::Beginner, 1);
        game.tick(5);
        assert_eq!(game.elapsed(), 0);

        game.reveal(40);
        game.tick(5);
        assert_eq!(game.elapsed(), 5);
        assert_eq!(game.readout().clock, "00:05");

        // A hidden tab reports one long delta; it counts, up to a cap.
        game.tick(MAX_TICK_SECONDS * 3);
        assert_eq!(game.elapsed(), 5 + MAX_TICK_SECONDS);

        let mut game = started(Level::Beginner, 7);
        let mine = mine_of(&game);
        game.reveal(mine);
        let stopped = game.elapsed();
        game.tick(30);
        assert_eq!(game.elapsed(), stopped);
    }

    #[test]
    fn the_clock_stops_at_its_maximum() {
        let mut game = started(Level::Beginner, 2);
        game.elapsed = MAX_ELAPSED - 1;
        game.tick(10);
        assert_eq!(game.elapsed(), MAX_ELAPSED);
        assert_eq!(game.readout().clock, "99:59:59");
    }

    #[test]
    fn the_clock_shows_hours_only_when_there_are_some() {
        assert_eq!(clock_words(0), "00:00");
        assert_eq!(clock_words(9), "00:09");
        assert_eq!(clock_words(59), "00:59");
        assert_eq!(clock_words(60), "01:00");
        assert_eq!(clock_words(3_599), "59:59");
        assert_eq!(clock_words(3_600), "1:00:00");
        assert_eq!(clock_words(3_661), "1:01:01");
    }

    #[test]
    fn a_duration_reads_as_prose() {
        assert_eq!(duration_words(1), "1 second");
        assert_eq!(duration_words(42), "42 seconds");
        assert_eq!(duration_words(60), "1 minute");
        assert_eq!(duration_words(61), "1 minute 1 second");
        assert_eq!(duration_words(125), "2 minutes 5 seconds");
        assert_eq!(duration_words(7_200), "120 minutes");
    }

    #[test]
    fn a_square_speaks_its_position_and_what_it_is() {
        let mut game = Game::new(Level::Beginner, 6);
        let cells = game.cells();
        assert_eq!(cells[0].label(), "Row 1, column 1, covered");
        assert_eq!(cells[0].face(), "");

        game.flag(0);
        assert_eq!(game.cells()[0].label(), "Row 1, column 1, flagged");
        assert_eq!(game.cells()[0].face(), "⚑");

        let game = started(Level::Beginner, 6);
        let cells = game.cells();
        let clear = cells
            .iter()
            .find(|cell| cell.cover == Cover::Revealed && cell.count == 0);
        let clear = clear.expect("a first click opens a clear square");
        assert!(clear.label().ends_with("clear"));

        let numbered = cells
            .iter()
            .find(|cell| cell.cover == Cover::Revealed && cell.count == 1);
        if let Some(numbered) = numbered {
            assert_eq!(numbered.face(), "1");
            assert!(numbered.label().ends_with("1 mine around"));
            assert!(numbered.classes().contains("n1"));
        }
    }

    #[test]
    fn a_number_colour_class_matches_the_count_and_only_on_a_number() {
        let game = started(Level::Intermediate, 8);
        for cell in game.cells() {
            match cell.count {
                0 | 9..=u8::MAX => assert!(!cell.classes().contains("n0")),
                count => assert_eq!(
                    cell.cover == Cover::Revealed,
                    cell.classes().contains(&format!("n{count}"))
                ),
            }
        }
        assert!(game
            .cells()
            .iter()
            .all(|cell| !cell.classes().contains("n0")));
    }

    #[test]
    fn a_game_round_trips_through_its_stored_form() {
        let mut game = started(Level::Beginner, 19);
        game.flag(0);
        game.reveal(40);
        game.tick(17);
        let stored = game.to_stored();
        let restored = Game::from_stored(&stored).expect("a game it wrote itself");
        assert_eq!(restored.level, game.level);
        assert_eq!(restored.seed, game.seed);
        assert_eq!(restored.first, game.first);
        assert_eq!(restored.status, game.status);
        assert_eq!(restored.elapsed, game.elapsed);
        assert_eq!(restored.cover, game.cover);
        assert_eq!(restored.flags, game.flags);
        assert_eq!(restored.exploded, game.exploded);
        assert_eq!(restored.field, game.field);
        assert_eq!(restored.to_stored(), stored);
    }

    #[test]
    fn a_finished_game_round_trips_too() {
        let mut game = started(Level::Beginner, 7);
        let mine = mine_of(&game);
        game.reveal(mine);
        let stored = game.to_stored();
        let restored = Game::from_stored(&stored).expect("a lost game is a game");
        assert_eq!(restored.status(), Status::Lost);
        assert_eq!(restored.exploded, Some(mine));
        assert_eq!(restored.cells(), game.cells());
    }

    #[test]
    fn a_new_game_round_trips_without_a_minefield() {
        let game = Game::new(Level::Expert, 42);
        let stored = game.to_stored();
        let restored = Game::from_stored(&stored).expect("an unstarted game is a game");
        assert_eq!(restored.status(), Status::Ready);
        assert!(restored.field.is_none());
        assert_eq!(restored.to_stored(), stored);
    }

    #[test]
    fn anything_that_is_not_a_stored_game_is_refused() {
        let valid = started(Level::Beginner, 3).to_stored();
        assert!(Game::from_stored(&valid).is_some());

        let refused = [
            String::new(),
            "v2|beginner|3|0|playing|0|".to_owned() + &".".repeat(81) + "|-",
            "v1|huge|3|0|playing|0|".to_owned() + &".".repeat(81) + "|-",
            "v1|beginner|-3|0|playing|0|".to_owned() + &".".repeat(81) + "|-",
            "v1|beginner|3|x|playing|0|".to_owned() + &".".repeat(81) + "|-",
            "v1|beginner|3|0|paused|0|".to_owned() + &".".repeat(81) + "|-",
            "v1|beginner|3|0|playing|-1|".to_owned() + &".".repeat(81) + "|-",
            // one square short, and one too long
            "v1|beginner|3|0|playing|0|".to_owned() + &".".repeat(80) + "|-",
            "v1|beginner|3|0|playing|0|".to_owned() + &".".repeat(82) + "|-",
            // a letter that means nothing
            "v1|beginner|3|0|playing|0|".to_owned() + &"X".repeat(81) + "|-",
            // the first click is off the board
            "v1|beginner|3|81|playing|0|".to_owned() + &".".repeat(81) + "|-",
            // a field with no first click
            "v1|beginner|3|-|playing|0|".to_owned() + &"R".repeat(81) + "|-",
            // a ready game that has been played
            "v1|beginner|3|0|ready|0|".to_owned() + &"R".repeat(81) + "|-",
            // a ready game with a first click
            "v1|beginner|3|0|ready|0|".to_owned() + &".".repeat(81) + "|-",
            // more flags than mines
            "v1|beginner|3|0|playing|0|".to_owned() + &"F".repeat(11) + &".".repeat(70) + "|-",
            // a trailing field
            (started(Level::Beginner, 3).to_stored() + "|extra"),
        ];
        for raw in refused {
            assert!(
                Game::from_stored(&raw).is_none(),
                "should have refused: {raw}"
            );
        }
    }

    #[test]
    fn a_stored_clock_past_the_maximum_is_clamped_not_refused() {
        let raw = format!("v1|beginner|3|0|playing|999999|{}|-", ".".repeat(81));
        let game = Game::from_stored(&raw).expect("a long clock is still a game");
        assert_eq!(game.elapsed(), MAX_ELAPSED);
    }

    #[test]
    fn every_level_reports_its_own_board() {
        for level in Level::ALL {
            let game = Game::new(level, 2);
            assert_eq!(game.cells().len(), level.cells());
            assert_eq!(game.cells().last().expect("cells").row, level.height());
            assert_eq!(game.cells().last().expect("cells").column, level.width());
        }
    }
}
