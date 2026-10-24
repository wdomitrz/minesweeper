//! The minefield: where the mines are, and what every square touches.
//!
//! Nothing here knows about clicks, flags or clocks — that is [`crate::game`].
//! This module answers two questions and nothing else: is this square a mine,
//! and how many of the (at most eight) squares around it are.
//!
//! The mines are placed on the **first reveal**, not when the game starts, and
//! the first reveal's square and its neighbours are left clear. That is the
//! safety rule every modern minesweeper has: a first click that can lose is a
//! game that loses players before they have made a decision. Placing them late
//! is what makes it possible without lying about the board afterwards.
//!
//! Placement is seeded rather than random from the inside, because this module
//! has no browser to ask for randomness and must stay testable: the seed comes
//! from the clock in `ui.rs`, and the same seed and first square give the same
//! board every time.

/// How big a board is and how many mines it hides.
///
/// The three classic sizes. Expert is 30 squares wide on purpose: it is the
/// board the game has been played on since 1992, and shrinking it to fit a
/// phone would make it a different game. The page scrolls that board sideways
/// instead.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Beginner,
    Intermediate,
    Expert,
    /// 99 squares square. Not a board anyone finishes; it is the size for
    /// someone who wants to find out how big a minefield their phone can hold,
    /// and it exists because a game that only offers three sizes someone else
    /// picked eventually runs into a fourth.
    Insane,
}

impl Level {
    /// Every level, easiest first: what the menu lists and what the tests walk.
    pub const ALL: [Self; 4] = [
        Self::Beginner,
        Self::Intermediate,
        Self::Expert,
        Self::Insane,
    ];

    /// The board's width in squares.
    pub const fn width(self) -> usize {
        match self {
            Self::Beginner => 9,
            Self::Intermediate => 16,
            Self::Expert => 30,
            Self::Insane => 99,
        }
    }

    /// The board's height in squares.
    pub const fn height(self) -> usize {
        match self {
            Self::Beginner => 9,
            Self::Intermediate => 16,
            Self::Expert => 16,
            Self::Insane => 99,
        }
    }

    /// How many mines the board hides.
    pub const fn mines(self) -> usize {
        match self {
            Self::Beginner => 10,
            Self::Intermediate => 40,
            Self::Expert => 99,
            // One mine in five, the density of a board that is dense enough to
            // need care and open enough to be played. Not scaled from Expert's
            // 20.6% into a rounder number by accident: 2000 is 20.4% of 9801.
            Self::Insane => 2_000,
        }
    }

    /// Every square on the board, whether covered or not.
    pub const fn cells(self) -> usize {
        self.width() * self.height()
    }

    /// The name the page shows for this level.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Beginner => "Beginner",
            Self::Intermediate => "Intermediate",
            Self::Expert => "Expert",
            Self::Insane => "Insane",
        }
    }

    /// The stable name this level is stored and compared under.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Beginner => "beginner",
            Self::Intermediate => "intermediate",
            Self::Expert => "expert",
            Self::Insane => "insane",
        }
    }

    /// The level a stored slug names, or `None` for anything else.
    ///
    /// Unknown input is `None` rather than a default: a stored game naming a
    /// level this build does not have is a game that should be discarded, not
    /// silently reopened as a different one.
    pub fn from_slug(slug: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|level| level.slug() == slug)
    }

    /// The largest board this build can make, which is the most squares a
    /// stored game can legitimately name.
    pub fn biggest() -> Self {
        Self::ALL
            .into_iter()
            .max_by_key(|level| level.cells())
            .unwrap_or(Self::Beginner)
    }
}

/// The squares touching `index`, in reading order and never off the board.
///
/// An iterator rather than a slice or a `Vec`: the reveal in [`crate::game`]
/// walks this for every square it opens, and a board with 480 squares would
/// allocate thousands of tiny vectors per click. The arithmetic is done once
/// here so the callers cannot get the edges wrong.
pub fn around(width: usize, height: usize, index: usize) -> Around {
    Around {
        width,
        height,
        row: index / width.max(1),
        column: index % width.max(1),
        step: 0,
    }
}

/// The iterator [`around`] returns. Eight squares at most, fewer at an edge.
pub struct Around {
    width: usize,
    height: usize,
    row: usize,
    column: usize,
    step: usize,
}

/// The eight directions, as (row, column) offsets, in reading order.
const STEPS: [(isize, isize); 8] = [
    (-1, -1),
    (-1, 0),
    (-1, 1),
    (0, -1),
    (0, 1),
    (1, -1),
    (1, 0),
    (1, 1),
];

impl Iterator for Around {
    type Item = usize;

    fn next(&mut self) -> Option<usize> {
        while self.step < STEPS.len() {
            let (down, right) = STEPS[self.step];
            self.step += 1;
            // The board is at most 30 squares wide, so these conversions are
            // exact and the clippy cast lints do not apply to a value whose
            // bound is a constant this module declares.
            let row = self.row as isize + down;
            let column = self.column as isize + right;
            if row < 0 || column < 0 || row as usize >= self.height || column as usize >= self.width
            {
                continue;
            }
            return Some(row as usize * self.width + column as usize);
        }
        None
    }
}

/// A board of mines, with the count of neighbouring mines precomputed.
///
/// The counts are computed once, at placement, rather than counted on every
/// reveal: a flood fill asks for the count of every square it touches, and
/// counting eight neighbours each time turns opening a corner of an expert
/// board into tens of thousands of comparisons.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Minefield {
    width: usize,
    height: usize,
    mine: Vec<bool>,
    count: Vec<u8>,
}

impl Minefield {
    /// A board with `level`'s mines, placed so that `first` and its neighbours
    /// are clear.
    ///
    /// `seed` makes the placement repeatable: the same seed and the same first
    /// square are the same board. That is what lets a saved game be rebuilt
    /// from four numbers instead of a list of 480 mine positions.
    ///
    /// # Panics
    ///
    /// If the level cannot hold its own mines beside a clear first square. No
    /// level is close to that — the tightest is Beginner, with 72 squares for
    /// 10 mines — and a board that cannot be built is a programming error, not
    /// something a player can cause.
    pub fn new(level: Level, seed: u64, first: usize) -> Self {
        let width = level.width();
        let height = level.height();
        let cells = width * height;
        assert!(first < cells, "the first click is on the board");

        let mut clear = vec![false; cells];
        clear[first] = true;
        for neighbour in around(width, height, first) {
            clear[neighbour] = true;
        }

        let mut candidates: Vec<usize> = (0..cells).filter(|index| !clear[*index]).collect();
        assert!(
            candidates.len() >= level.mines(),
            "{} cannot hold {} mines beside a safe first square",
            level.label(),
            level.mines()
        );
        shuffle(&mut candidates, seed);

        let mut mine = vec![false; cells];
        for index in candidates.into_iter().take(level.mines()) {
            mine[index] = true;
        }

        let mut count = vec![0u8; cells];
        for index in 0..cells {
            if mine[index] {
                continue;
            }
            let neighbours = around(width, height, index)
                .filter(|other| mine[*other])
                .count();
            // At most eight neighbours, so the count always fits a `u8`.
            count[index] = u8::try_from(neighbours).unwrap_or(8);
        }

        Self {
            width,
            height,
            mine,
            count,
        }
    }

    /// The board's width in squares.
    pub fn width(&self) -> usize {
        self.width
    }

    /// The board's height in squares.
    pub fn height(&self) -> usize {
        self.height
    }

    /// Every square on the board.
    pub fn cells(&self) -> usize {
        self.width * self.height
    }

    /// Whether this square holds a mine.
    pub fn is_mine(&self, index: usize) -> bool {
        self.mine.get(index).copied().unwrap_or(false)
    }

    /// How many mines touch this square: 0 to 8.
    pub fn count(&self, index: usize) -> u8 {
        self.count.get(index).copied().unwrap_or(0)
    }

    /// How many mines the board holds.
    pub fn mines(&self) -> usize {
        self.mine.iter().filter(|mine| **mine).count()
    }
}

/// A seeded pseudo-random generator, so placement is repeatable and testable.
///
/// `xorshift64*`: three shifts and a multiply, no dependency, and good enough
/// to shuffle a few hundred squares. It is not for anything secret; the only
/// thing it protects is which squares of a minesweeper board are mined.
struct Rng(u64);

impl Rng {
    /// A generator for `seed`. Zero is remapped because xorshift cannot leave
    /// it: a zero state would produce zero forever and one board for the rest
    /// of time.
    fn seeded(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    /// The next value in the sequence.
    fn next(&mut self) -> u64 {
        let mut state = self.0;
        state ^= state >> 12;
        state ^= state << 25;
        state ^= state >> 27;
        self.0 = state;
        state.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// A value in `0..below`, uniformly.
    ///
    /// Rejection rather than a plain modulo: a modulo makes the first few
    /// values more likely by however much the range does not divide the word,
    /// and a minesweeper board is small enough for that to be visible as mines
    /// clustering in the top-left corner.
    fn below(&mut self, below: usize) -> usize {
        let range = u64::try_from(below).unwrap_or(1).max(1);
        let limit = u64::MAX - (u64::MAX % range);
        loop {
            let value = self.next();
            if value < limit {
                return usize::try_from(value % range).unwrap_or(0);
            }
        }
    }
}

/// Shuffle `items` in place with `seed`, by Fisher and Yates.
fn shuffle(items: &mut [usize], seed: u64) {
    let mut rng = Rng::seeded(seed);
    for last in (1..items.len()).rev() {
        let swap_with = rng.below(last + 1);
        items.swap(last, swap_with);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mines(field: &Minefield) -> Vec<usize> {
        (0..field.cells()).filter(|i| field.is_mine(*i)).collect()
    }

    #[test]
    fn every_level_is_the_board_it_claims_to_be() {
        assert_eq!((Level::Beginner.width(), Level::Beginner.height()), (9, 9));
        assert_eq!(Level::Beginner.cells(), 81);
        assert_eq!(Level::Beginner.mines(), 10);

        assert_eq!(
            (Level::Intermediate.width(), Level::Intermediate.height()),
            (16, 16)
        );
        assert_eq!(Level::Intermediate.mines(), 40);

        assert_eq!((Level::Expert.width(), Level::Expert.height()), (30, 16));
        assert_eq!(Level::Expert.cells(), 480);
        assert_eq!(Level::Expert.mines(), 99);

        assert_eq!((Level::Insane.width(), Level::Insane.height()), (99, 99));
        assert_eq!(Level::Insane.cells(), 9_801);
        assert_eq!(Level::Insane.mines(), 2_000);
    }

    #[test]
    fn every_level_can_actually_be_built_with_a_safe_first_click() {
        // The rule `Minefield::new` asserts, asserted for the whole list rather
        // than discovered by a panic in the biggest case: a 99x99 board with its
        // safe zone is still 9792 squares, and a level whose mines did not fit
        // would be a board that cannot start.
        for level in Level::ALL {
            let field = Minefield::new(level, 1, level.cells() / 2);
            assert_eq!(field.mines(), level.mines(), "{}", level.label());
        }
    }

    #[test]
    fn the_biggest_board_is_built_quickly_enough_to_start() {
        // Placement is one pass over the board plus a shuffle, so it is linear
        // in the squares. This is a floor, not a benchmark: a quadratic placement
        // would take minutes here and the app would look hung on launch.
        let start = std::time::Instant::now();
        let field = Minefield::new(Level::Insane, 12_345, 4_950);
        let elapsed = start.elapsed();
        assert_eq!(field.mines(), 2_000);
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "placing 2000 mines on 9801 squares took {elapsed:?}"
        );
    }

    #[test]
    fn every_level_has_room_for_its_mines_beside_a_clear_first_square() {
        for level in Level::ALL {
            // The safe zone is the first square plus, at worst, eight around it.
            assert!(
                level.cells() - 9 >= level.mines(),
                "{} has no room for its mines",
                level.label()
            );
        }
    }

    #[test]
    fn a_new_board_holds_exactly_the_declared_number_of_mines() {
        for level in Level::ALL {
            let field = Minefield::new(level, 12_345, level.cells() / 2);
            assert_eq!(field.mines(), level.mines(), "{}", level.label());
            assert_eq!(mines(&field).len(), level.mines(), "{}", level.label());
        }
    }

    #[test]
    fn the_first_square_and_its_neighbours_are_never_mined() {
        for level in Level::ALL {
            let width = level.width();
            let height = level.height();
            // A corner, an edge, the middle and the last square: every shape of
            // neighbourhood the safety rule has to cover.
            let corners = [0, width - 1, (height - 1) * width, level.cells() - 1];
            let edge = width + 3;
            let middle = (height / 2) * width + width / 2;
            for first in corners.into_iter().chain([edge, middle]) {
                let field = Minefield::new(level, 98_765, first);
                assert!(!field.is_mine(first), "{}", level.label());
                let mut zone = 0;
                for neighbour in around(width, height, first) {
                    zone += 1;
                    assert!(
                        !field.is_mine(neighbour),
                        "{}: {neighbour} touches the safe first square {first}",
                        level.label()
                    );
                }
                assert!(zone > 0, "a square must have at least one neighbour");
            }
        }
    }

    #[test]
    fn the_counts_match_the_mines_a_plain_count_finds() {
        let level = Level::Intermediate;
        let field = Minefield::new(level, 5_150, 100);
        for index in 0..field.cells() {
            if field.is_mine(index) {
                // A mine's own count is not shown; it stays zero.
                assert_eq!(field.count(index), 0);
                continue;
            }
            let expected = around(field.width(), field.height(), index)
                .filter(|other| field.is_mine(*other))
                .count();
            assert_eq!(
                usize::from(field.count(index)),
                expected,
                "count at {index}"
            );
        }
    }

    #[test]
    fn no_square_counts_more_than_eight() {
        for level in Level::ALL {
            let field = Minefield::new(level, 1, 0);
            for index in 0..field.cells() {
                assert!(field.count(index) <= 8);
            }
        }
    }

    #[test]
    fn the_same_seed_and_first_square_build_the_same_board() {
        for level in Level::ALL {
            let one = Minefield::new(level, 777, 3);
            let other = Minefield::new(level, 777, 3);
            assert_eq!(one, other, "{}", level.label());
        }
    }

    #[test]
    fn a_different_seed_builds_a_different_board() {
        let one = Minefield::new(Level::Expert, 1, 50);
        let other = Minefield::new(Level::Expert, 2, 50);
        assert_ne!(one, other);
    }

    #[test]
    fn a_different_first_square_builds_a_different_board() {
        let one = Minefield::new(Level::Beginner, 424_242, 0);
        let other = Minefield::new(Level::Beginner, 424_242, 80);
        assert_ne!(one, other);
    }

    #[test]
    fn the_neighbours_of_a_corner_number_three() {
        let field = Minefield::new(Level::Beginner, 9, 0);
        let width = field.width();
        let height = field.height();
        let neighbours: Vec<usize> = around(width, height, 0).collect();
        assert_eq!(neighbours.len(), 3);
        assert_eq!(neighbours, vec![1, width, width + 1]);
    }

    #[test]
    fn the_neighbours_of_a_middle_square_number_eight() {
        let width = 9;
        let height = 9;
        let index = 4 * width + 4;
        let neighbours: Vec<usize> = around(width, height, index).collect();
        assert_eq!(neighbours.len(), 8);
        assert_eq!(neighbours, vec![30, 31, 32, 39, 41, 48, 49, 50]);
    }

    #[test]
    fn the_neighbours_of_an_edge_square_number_five() {
        let width = 9;
        let height = 9;
        let index = 4; // first row, middle column
        let neighbours: Vec<usize> = around(width, height, index).collect();
        assert_eq!(neighbours.len(), 5);
        assert!(neighbours.iter().all(|n| *n < 18));
    }

    #[test]
    fn neighbours_are_always_inside_the_board() {
        for level in Level::ALL {
            let width = level.width();
            let height = level.height();
            for index in 0..level.cells() {
                for neighbour in around(width, height, index) {
                    assert!(neighbour < level.cells());
                    assert_ne!(neighbour, index);
                    let row = index / width;
                    let column = index % width;
                    let other_row = neighbour / width;
                    let other_column = neighbour % width;
                    let distance_row = row.abs_diff(other_row);
                    let distance_column = column.abs_diff(other_column);
                    assert!(distance_row <= 1 && distance_column <= 1);
                    assert!(distance_row + distance_column > 0);
                }
            }
        }
    }

    #[test]
    fn the_generator_refuses_a_zero_seed_and_still_moves() {
        // A zero seed must not pin the board to one layout forever: xorshift
        // cannot leave a zero state, so the seed is remapped before it is used.
        let one = Minefield::new(Level::Beginner, 0, 40);
        assert_eq!(one.mines(), 10);

        let mut rng = Rng::seeded(0);
        let first = rng.next();
        let second = rng.next();
        assert_ne!(first, 0);
        assert_ne!(first, second);
    }

    #[test]
    fn a_shuffle_keeps_every_item_and_changes_the_order() {
        let mut items: Vec<usize> = (0..100).collect();
        shuffle(&mut items, 3_141);
        let mut sorted = items.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..100).collect::<Vec<_>>());
        assert_ne!(items, (0..100).collect::<Vec<_>>());
    }

    #[test]
    fn the_level_slugs_round_trip_and_refuse_anything_else() {
        for level in Level::ALL {
            assert_eq!(Level::from_slug(level.slug()), Some(level));
        }
        assert_eq!(Level::from_slug(""), None);
        assert_eq!(Level::from_slug("Expert"), None);
        assert_eq!(Level::from_slug("beginner "), None);
    }

    #[test]
    fn the_biggest_level_is_the_one_with_the_most_squares() {
        // Derived, not asserted by name: a level list that grows must not leave
        // a "biggest" behind that is no longer the biggest.
        let biggest = Level::ALL
            .into_iter()
            .max_by_key(|level| level.cells())
            .expect("a level list is not empty");
        assert_eq!(Level::biggest(), biggest);
        assert_eq!(biggest, Level::Insane);
    }
}
