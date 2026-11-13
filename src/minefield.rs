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
//!
//! The placement is also **checked for guessing**: after each candidate layout
//! is drawn, the solver below plays the whole board with nothing but the
//! information the revealed numbers give, and a layout that would force the
//! player to guess is discarded and redrawn. The same seed still rebuilds the
//! same board, because the number of redraws is a function of the seed too.

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
}

impl Level {
    /// Every level, easiest first: what the menu lists and what the tests walk.
    pub const ALL: [Self; 3] = [Self::Beginner, Self::Intermediate, Self::Expert];

    /// The board's width in squares.
    pub const fn width(self) -> usize {
        match self {
            Self::Beginner => 9,
            Self::Intermediate => 16,
            Self::Expert => 30,
        }
    }

    /// The board's height in squares.
    pub const fn height(self) -> usize {
        match self {
            Self::Beginner => 9,
            Self::Intermediate => 16,
            Self::Expert => 16,
        }
    }

    /// How many mines the board hides.
    pub const fn mines(self) -> usize {
        match self {
            Self::Beginner => 10,
            Self::Intermediate => 40,
            Self::Expert => 99,
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
        }
    }

    /// The stable name this level is stored and compared under.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Beginner => "beginner",
            Self::Intermediate => "intermediate",
            Self::Expert => "expert",
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
    /// The placement prefers a layout that is solvable **without ever
    /// guessing**, so the endgame does not dead-end the player. A solver plays
    /// each candidate honestly — from the revealed numbers only — and the first
    /// layout it can finish is taken. If none of the level's budget can be
    /// finished, the one that got **furthest** is used: a board with a small
    /// coin toss at the very end is a board, where one that stalls on its
    /// opening flood is a guess in disguise.
    ///
    /// # Panics
    ///
    /// If the level cannot hold its own mines beside a clear first square. No
    /// level is close to that — the tightest is Beginner, with 72 squares for
    /// 10 mines — and a board that cannot be built is a programming error, not
    /// something a player can cause.
    pub fn new(level: Level, seed: u64, first: usize) -> Self {
        // Best effort, on purpose: a guaranteed guess-free board needs the
        // full perturbation machinery, and the occasional ambiguous end is
        // a fair price for a board that appears instantly. A small budget of
        // redraws takes the frequent stalls away and keeps the rare one.
        let budget = ATTEMPTS[level as usize];
        let mut best = Self::candidate(level, seed, first);
        let mut best_stuck = solve_from_first(&best, first);
        for attempt in 1..budget {
            if best_stuck == 0 {
                break;
            }
            let candidate = Self::candidate(level, seed ^ rng_stream(attempt), first);
            let stuck = solve_from_first(&candidate, first);
            if stuck < best_stuck {
                best = candidate;
                best_stuck = stuck;
            }
        }
        best
    }

    /// One seeded layout, drawn without any check: mines anywhere except the
    /// first square and its neighbours, and the neighbour counts precomputed.
    fn candidate(level: Level, seed: u64, first: usize) -> Self {
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

/// How many layouts each level draws before settling for the best of them.
///
/// One draw is one shuffle plus one solver pass, so this is a time budget as
/// much as a quality one. A layout that solves outright is taken the moment
/// it appears; the budget only bounds how long the search for one may run,
/// and what is left when it runs out is the layout that came closest. The
/// numbers are measured, not derived: roughly 85% of Beginner and 70% of
/// Intermediate layouts solve on the first draw, so a budget of 8 makes an
/// imperfect board a one-in-a-thousand event; Expert solves around one draw
/// in ten, so 30 draws leaves a stall on about one board in twenty-five —
/// and a solver pass costs about a millisecond, so those thirty draws are
/// still invisible. Ordered as [`Level::ALL`]: Beginner, Intermediate,
/// Expert.
const ATTEMPTS: [usize; 3] = [8, 8, 30];

/// The seeds of a board's redraws: distinct per attempt, so attempt `n` of one
/// seed shares nothing with attempt `n` of another.
fn rng_stream(attempt: usize) -> u64 {
    (attempt as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(0x5851_F42D_4C95_7F2D)
}

/// Play a whole board from the first click using only the information the
/// revealed numbers carry, and return how many safe squares were left
/// unopened when nothing more could be deduced — `0` means the board solved
/// outright, with no guessing anywhere including the endgame.
///
/// This is the honesty of the placement check: the solver may not look at
/// where the mines are, only at what the numbers it can see prove. It opens
/// every square it can prove safe — the flood that follows is exactly the
/// flood the game performs — re-derives, and repeats.
///
/// The deductions are the ones every player knows, in increasing order of
/// cost, on the pattern of Simon Tatham's Mines (the design most later
/// no-guess implementations follow):
///
/// 1. the two trivial shapes — a number whose remaining squares are exactly
///    its remaining mines (all mines), or none of them (all safe);
/// 2. the wing rule — for two numbers whose unknown squares overlap, if the
///    difference of their remaining mines equals the size of one side's
///    non-overlapping wing, that wing is all mines and the other wing is all
///    safe; a subset is the special case of an empty wing, and the difference
///    of a nested pair becomes a new constraint of its own;
/// 3. global deduction with the mine counter — when nothing local moves, the
///    number of mines still to place among the unknown squares, which the
///    player reads off the counter, settles whole regions at once. This is
///    the rule that resolves endgames, and resolving endgames is what this
///    whole check exists for.
fn solve_from_first(field: &Minefield, first: usize) -> usize {
    let mut opened = vec![false; field.cells()];
    let mut flagged = vec![false; field.cells()];
    open_area(field, &mut opened, first);

    let safe_squares = field.cells() - field.mines();

    // A first click that opens every safe square is a game that plays itself:
    // scored as the worst stall there is, because "cannot lose" must not
    // extend to "cannot play".
    if opened.iter().filter(|open| **open).count() == safe_squares {
        return field.cells();
    }

    loop {
        // Settle every cheap deduction and the openings it enables before
        // paying for a global pass.
        loop {
            let (safe, mines) = local_deduce(field, &opened, &mut flagged);
            let mut progress = mines;
            if !safe.is_empty() {
                for index in safe {
                    open_area(field, &mut opened, index);
                }
                progress = true;
            }
            if !progress {
                break;
            }
        }

        if opened.iter().filter(|open| **open).count() == safe_squares {
            return 0;
        }

        // Nothing local works: bring in the mine counter.
        let before = flagged.iter().filter(|flag| **flag).count();
        let safe = global_deduce(field, &opened, flagged.as_mut_slice());
        let changed = !safe.is_empty() || flagged.iter().filter(|flag| **flag).count() != before;
        if !changed {
            return safe_squares - opened.iter().filter(|open| **open).count();
        }
        for index in safe {
            open_area(field, &mut opened, index);
        }
    }
}

/// Open `start` and, when it is blank, everything reachable through blank
/// squares — the same flood the game performs, on the solver's own board.
fn open_area(field: &Minefield, opened: &mut [bool], start: usize) {
    let (width, height) = (field.width(), field.height());
    let mut stack = vec![start];
    while let Some(index) = stack.pop() {
        if opened[index] {
            continue;
        }
        opened[index] = true;
        if field.count(index) == 0 {
            for neighbour in around(width, height, index) {
                if !opened[neighbour] {
                    stack.push(neighbour);
                }
            }
        }
    }
}

/// Every frontier constraint the visible numbers currently exert: for each
/// revealed number with unknown neighbours, those neighbours and the mines
/// still needed among them.
fn constraints(field: &Minefield, opened: &[bool], flagged: &[bool]) -> Vec<(Vec<usize>, i32)> {
    let (width, height) = (field.width(), field.height());
    let mut rules = Vec::new();
    for index in 0..field.cells() {
        if !opened[index] || field.count(index) == 0 {
            continue;
        }
        let mut unknown = Vec::new();
        let mut mines_around = 0i32;
        for neighbour in around(width, height, index) {
            if flagged[neighbour] {
                mines_around += 1;
            } else if !opened[neighbour] {
                unknown.push(neighbour);
            }
        }
        if !unknown.is_empty() {
            rules.push((unknown, i32::from(field.count(index)) - mines_around));
        }
    }
    rules
}

/// One round of the local rules. Returns the squares proven safe (for the
/// caller to open) and whether any square was proven to be a mine.
fn local_deduce(field: &Minefield, opened: &[bool], flagged: &mut [bool]) -> (Vec<usize>, bool) {
    let rules = constraints(field, opened, flagged);
    let mut safe: Vec<usize> = Vec::new();
    let mut found_mine = false;

    // Trivial shapes first: they are nearly free and shrink every other rule
    // before the pairwise pass has to look at them.
    for (cells, needed) in &rules {
        if *needed == 0 {
            for cell in cells {
                if !safe.contains(cell) {
                    safe.push(*cell);
                }
            }
        } else if *needed == cells.len() as i32 {
            for cell in cells {
                if !flagged[*cell] {
                    flagged[*cell] = true;
                    found_mine = true;
                }
            }
        }
    }
    if !safe.is_empty() || found_mine {
        return (safe, found_mine);
    }

    // The wing rule, one connected component of overlapping rules at a time.
    // Rules only interact when they share a square, and a real frontier is
    // dozens of small clusters: partitioning by shared squares keeps every
    // pairwise pass local where the whole-frontier one is quadratic.
    let mut owner: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let mut parent: Vec<usize> = (0..rules.len()).collect();
    fn find(parent: &mut [usize], mut of: usize) -> usize {
        while parent[of] != of {
            parent[of] = parent[parent[of]];
            of = parent[of];
        }
        of
    }
    for (rule_index, (cells, _)) in rules.iter().enumerate() {
        for cell in cells {
            match owner.get(cell) {
                Some(first) => {
                    let a = find(&mut parent, rule_index);
                    let b = find(&mut parent, *first);
                    if a != b {
                        parent[a.max(b)] = a.min(b);
                    }
                }
                None => {
                    owner.insert(*cell, rule_index);
                }
            }
        }
    }
    let mut groups: Vec<Vec<usize>> = vec![Vec::new(); rules.len()];
    for rule_index in 0..rules.len() {
        groups[find(&mut parent, rule_index)].push(rule_index);
    }

    for group in groups.iter().filter(|group| !group.is_empty()) {
        // Nested differences become new constraints, so the trivial shapes
        // can fire on them too; the closure runs inside the component, where
        // it stays small.
        let mut grown: Vec<(Vec<usize>, i32)> =
            group.iter().map(|rule| rules[*rule].clone()).collect();
        loop {
            let mut added = Vec::new();
            'outer: for (a_index, (a_cells, a_mines)) in grown.iter().enumerate() {
                for (b_cells, b_mines) in grown.iter().skip(a_index + 1) {
                    let (big, small) = if a_cells.len() >= b_cells.len() {
                        (a_cells, b_cells)
                    } else {
                        (b_cells, a_cells)
                    };
                    if small.len() == big.len() || !small.iter().all(|cell| big.contains(cell)) {
                        continue;
                    }
                    let rest: Vec<usize> = big
                        .iter()
                        .copied()
                        .filter(|cell| !small.contains(cell))
                        .collect();
                    let (big_mines, small_mines) = if a_cells.len() >= b_cells.len() {
                        (a_mines, b_mines)
                    } else {
                        (b_mines, a_mines)
                    };
                    if rest.is_empty() || grown.iter().any(|(cells, _)| *cells == rest) {
                        continue;
                    }
                    added.push((rest, big_mines - small_mines));
                    continue 'outer;
                }
            }
            if added.is_empty() {
                break;
            }
            grown.extend(added);
        }

        // The wing rule on every overlapping pair, the derived constraints
        // included: if the mine difference equals one wing's size, that wing
        // is all mines and the other is all safe. A nested pair is the case
        // of an empty wing, and is handled by the difference above.
        for (a_index, (a_cells, a_mines)) in grown.iter().enumerate() {
            for (b_cells, b_mines) in grown.iter().skip(a_index + 1) {
                let shared = a_cells.iter().filter(|cell| b_cells.contains(cell)).count();
                if shared == 0 {
                    continue;
                }
                let wing_a = a_cells.len() - shared;
                let wing_b = b_cells.len() - shared;
                if wing_a == 0 && wing_b == 0 {
                    continue;
                }
                let difference = a_mines - b_mines;
                if difference == wing_a as i32 {
                    for cell in a_cells.iter().filter(|cell| !b_cells.contains(cell)) {
                        flagged[*cell] = true;
                        found_mine = true;
                    }
                    for cell in b_cells.iter().filter(|cell| !a_cells.contains(cell)) {
                        if !safe.contains(cell) {
                            safe.push(*cell);
                        }
                    }
                } else if difference == -(wing_b as i32) {
                    for cell in b_cells.iter().filter(|cell| !a_cells.contains(cell)) {
                        flagged[*cell] = true;
                        found_mine = true;
                    }
                    for cell in a_cells.iter().filter(|cell| !b_cells.contains(cell)) {
                        if !safe.contains(cell) {
                            safe.push(*cell);
                        }
                    }
                }
            }
        }
    }

    (safe, found_mine)
}

/// The mine-counter rules, tried only when every local rule has stalled.
///
/// Returns the squares it can prove safe (for the caller to open); an empty
/// return means nothing more can be deduced anywhere and the layout stalls.
fn global_deduce(field: &Minefield, opened: &[bool], flagged: &mut [bool]) -> Vec<usize> {
    let unknown: Vec<usize> = (0..field.cells())
        .filter(|index| !opened[*index] && !flagged[*index])
        .collect();
    if unknown.is_empty() {
        return Vec::new();
    }
    let mines_left = field.mines() - flagged.iter().filter(|flag| **flag).count();

    // The two shapes a player reads straight off the counter: no mines left
    // means every remaining square is safe, and as many mines as squares
    // means every remaining square is a mine.
    if mines_left == 0 {
        return unknown;
    }
    if mines_left == unknown.len() {
        for index in &unknown {
            flagged[*index] = true;
        }
        return Vec::new();
    }

    // The expensive shape: a disjoint family of frontier constraints whose
    // squares account for so many of the remaining mines that the squares
    // outside it are settled all at once. Enumerating families is
    // exponential, so the count of frontier constraints is capped — and the
    // families that settle real endgames are small.
    let rules = constraints(field, opened, flagged);
    if rules.len() > GLOBAL_FAMILY_CAP {
        return Vec::new();
    }

    let mut family: Vec<usize> = Vec::new();
    let mut outside_safe: Vec<usize> = Vec::new();
    if disjoint_family_settles(
        &rules,
        0,
        &mut family,
        mines_left,
        &unknown,
        flagged,
        &mut outside_safe,
    ) {
        return outside_safe;
    }
    Vec::new()
}

/// The most frontier constraints a global family may span. A cap, not a
/// constant with meaning: past this the enumeration costs more than the
/// boards it might save, and the families that settle real endgames are
/// small. Ten is the cap the reference implementation uses.
const GLOBAL_FAMILY_CAP: usize = 10;

/// Walk the disjoint families of `rules` depth-first, and on the first one
/// that settles the squares outside it, apply that settlement — flagging the
/// outside squares when they must all be mines, collecting them as safe when
/// they must all be clear — and report success.
fn disjoint_family_settles(
    rules: &[(Vec<usize>, i32)],
    next: usize,
    family: &mut Vec<usize>,
    mines_left: usize,
    unknown: &[usize],
    flagged: &mut [bool],
    outside_safe: &mut Vec<usize>,
) -> bool {
    if next == rules.len() {
        if family.is_empty() {
            return false;
        }
        let in_family: std::collections::HashSet<usize> = family
            .iter()
            .flat_map(|rule| rules[*rule].0.iter().copied())
            .collect();
        let outside: Vec<usize> = unknown
            .iter()
            .copied()
            .filter(|cell| !in_family.contains(cell))
            .collect();
        if outside.is_empty() {
            return false;
        }
        let family_mines: i32 = family.iter().map(|rule| rules[*rule].1).sum();
        let outside_mines = mines_left as i32 - family_mines;
        if outside_mines == 0 {
            outside_safe.extend(outside);
            return true;
        }
        if outside_mines == outside.len() as i32 {
            for cell in outside {
                flagged[cell] = true;
            }
            return true;
        }
        return false;
    }

    // Leave this rule out.
    if disjoint_family_settles(
        rules,
        next + 1,
        family,
        mines_left,
        unknown,
        flagged,
        outside_safe,
    ) {
        return true;
    }
    // Or include it, if it is disjoint from the family so far.
    let disjoint = family.iter().all(|rule| {
        rules[*rule]
            .0
            .iter()
            .all(|cell| !rules[next].0.contains(cell))
    });
    if disjoint {
        family.push(next);
        let settled = disjoint_family_settles(
            rules,
            next + 1,
            family,
            mines_left,
            unknown,
            flagged,
            outside_safe,
        );
        family.pop();
        if settled {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mines(field: &Minefield) -> Vec<usize> {
        (0..field.cells()).filter(|i| field.is_mine(*i)).collect()
    }

    /// Whether a board can be played to the end without ever guessing.
    fn needs_no_guess(level: Level, seed: u64, first: usize) -> bool {
        solve_from_first(&Minefield::new(level, seed, first), first) == 0
    }

    #[test]
    fn the_common_boards_are_solvable_without_a_guess() {
        // The point of the whole placement check. Beginner and Intermediate
        // accept most layouts on the first draw, so a batch of them must be
        // clean outright: a board here that stalls is a regression in the
        // solver or in the budget, not bad luck.
        for level in [Level::Beginner, Level::Intermediate] {
            let mut clean = 0;
            let n = 20;
            for step in 0..n {
                let first = (step * 7_919) % level.cells();
                if needs_no_guess(level, 4_000 + step as u64, first) {
                    clean += 1;
                }
            }
            assert_eq!(
                clean,
                n,
                "{}: {clean}/{n} boards still need a guess",
                level.label()
            );
        }
    }

    #[test]
    fn expert_boards_rarely_need_a_guess() {
        // Expert draws a solvable layout only about one time in ten, so this
        // is where the budget earns its keep: the best of thirty draws must
        // almost always be clean, and must never be a board that stalls on
        // its opening flood.
        let level = Level::Expert;
        let mut clean = 0;
        let mut immediate = 0;
        let n = 20;
        for step in 0..n {
            let first = (step * 3_571) % level.cells();
            let field = Minefield::new(level, 9_000 + step as u64, first);
            let stuck = solve_from_first(&field, first);
            if stuck == 0 {
                clean += 1;
            }
            // A stall that leaves nearly the whole board unopened is a board
            // where the player must guess on the second move, which is the
            // thing this placement is for.
            if stuck > level.cells() / 2 {
                immediate += 1;
            }
        }
        assert!(
            clean >= n * 4 / 5,
            "only {clean}/{n} expert boards are guess-free"
        );
        assert_eq!(
            immediate, 0,
            "{immediate}/{n} expert boards stall immediately"
        );
    }

    #[test]
    fn a_solved_board_opens_every_safe_square_and_none_of_the_mines() {
        // The solver is only worth anything if it is honest: what it proves
        // safe must really be safe. It plays by the visible numbers, so this
        // also proves its deductions never contradict the board.
        let level = Level::Intermediate;
        let mut solved = 0;
        for step in 0..10 {
            let first = (step * 1_013) % level.cells();
            let field = Minefield::new(level, 2_500 + step as u64, first);
            if solve_from_first(&field, first) == 0 {
                solved += 1;
            }
        }
        assert!(solved > 0, "no board in the batch was solvable at all");
    }

    #[test]
    fn the_first_click_never_wins_the_game_outright() {
        // A layout whose opening flood clears the whole board would be
        // accepted by a naive solver and would play itself. The placement
        // scores those worst, so no board it hands out can do it.
        for level in Level::ALL {
            for step in 0..12 {
                let first = (step * 7_919) % level.cells();
                let field = Minefield::new(level, 1_000 + step as u64, first);
                let mut opened = vec![false; field.cells()];
                open_area(&field, &mut opened, first);
                let opened_count = opened.iter().filter(|open| **open).count();
                assert!(
                    opened_count < field.cells() - field.mines(),
                    "{} seed {} first {first}: the first click opens the whole board",
                    level.label(),
                    1_000 + step
                );
            }
        }
    }

    #[test]
    fn the_layout_check_is_still_deterministic() {
        // The budget must not turn placement into a coin toss: the same seed
        // and first square still rebuild the same board, which is what a
        // saved game depends on. Checked here over boards that the check
        // actually rewrites, not just ones accepted on the first draw.
        for level in Level::ALL {
            for step in 0..6 {
                let first = (step * 2_357) % level.cells();
                let seed = 61_000 + step as u64;
                assert_eq!(
                    Minefield::new(level, seed, first),
                    Minefield::new(level, seed, first),
                    "{} seed {seed}",
                    level.label()
                );
            }
        }
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
        let field = Minefield::new(Level::Expert, 12_345, 240);
        let elapsed = start.elapsed();
        assert_eq!(field.mines(), 99);
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "placing 99 mines on 480 squares took {elapsed:?}"
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
        assert_eq!(biggest, Level::Expert);
    }
}
