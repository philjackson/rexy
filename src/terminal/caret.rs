//! Selecting with the keyboard, as Vim's visual mode does: a caret moved
//! through all of a pane's screen and history, and the place a selection is
//! stretched from. Ghostty keeps both with their text as output arrives and
//! history scrolls away, as it keeps the selection itself.
use anyhow::Result;
use libghostty_vt::{
    Terminal,
    grid_ref::TrackedGridRef,
    point::{Coordinate, Point, Space},
    screen::{CellWide, Screen},
    selection::{SelectLineOptions, Selection},
    terminal::ScrollViewport,
};

/// A move of the caret.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Motion {
    Left,
    Right,
    Up,
    Down,
    /// To the start of the next word, to the start of this one or the one
    /// before, or to the end of this one or the next. `long` words are parted
    /// by blanks alone, as Vim's WORDs are. Others are parted also where
    /// letters meet punctuation.
    WordNext {
        long: bool,
    },
    WordBack {
        long: bool,
    },
    WordEnd {
        long: bool,
    },
    /// To the first column, to the first that is not blank, or to the last
    /// that is not.
    LineStart,
    LineText,
    LineEnd,
    /// To the first line of all, and to the last that has anything on it.
    Top,
    Bottom,
    /// To the first line in view, the middle one, and the last.
    ViewTop,
    ViewMiddle,
    ViewBottom,
    /// Half a view down at a time, or up when negative. The view moves as
    /// far, as Vim's moves with Ctrl+D and Ctrl+U.
    Halves(i32),
}

/// How a selection reaches from where it was begun to the caret.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Shape {
    /// Through the text between them.
    Chars,
    /// Through all of their lines and those between.
    Lines,
    /// Through the columns between them, on their lines and those between.
    Block,
}

/// What is asked of the caret.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Mark {
    /// Show it: at the end of what is selected, if anything is, which it then
    /// stretches. Otherwise at the terminal's cursor, where that is in view,
    /// or else at the start of the last line in view.
    Begin,
    Move(Motion),
    /// Select from where the caret is, in this shape, as it moves. Already
    /// selecting in this shape, select nothing. In another, change to this.
    Stretch(Shape),
    /// Have the caret at the other end of what is selected.
    Swap,
    /// Have what is selected copied, or the caret's line where nothing is.
    Yank,
    /// Put the caret away, and let go of what is selected.
    End,
}

/// A pane's caret, while there is one.
#[derive(Default)]
pub struct Caret {
    at: Option<TrackedGridRef>,
    from: Option<(TrackedGridRef, Shape)>,
    /// The screen it is on. The places it keeps are that screen's, and mean
    /// nothing on the other.
    screen: Screen,
}

impl Caret {
    /// Where the caret is in view: None while there is none, and Some(None)
    /// while it is out of view.
    pub fn viewed(&self) -> Result<Option<Option<(u16, u16)>>> {
        let Some(at) = &self.at else {
            return Ok(None);
        };
        Ok(Some(at.point(Space::Viewport)?.map(|at| (at.x, at.y as u16))))
    }

    pub fn mark(&mut self, terminal: &mut Terminal<'static, '_>, mark: Mark) -> Result<()> {
        self.follow(terminal)?;
        let Some(at) = &mut self.at else {
            if mark == Mark::Begin {
                let start = start(terminal)?;
                self.screen = terminal.active_screen()?;
                self.at = Some(terminal.track_grid_ref(Point::Screen(start))?);
                self.adopt(terminal, None)?;
                self.show(terminal)?;
            }
            return Ok(());
        };
        let here = point(at);
        match mark {
            Mark::Begin => {}
            Mark::Move(motion) => {
                let to = moved(terminal, here, motion)?;
                at.set(terminal, Point::Screen(to))?;
            }
            Mark::Stretch(shape) => match &mut self.from {
                Some((_, was)) if *was == shape => self.from = None,
                Some((_, was)) => *was = shape,
                None => self.from = Some((terminal.track_grid_ref(Point::Screen(here))?, shape)),
            },
            Mark::Swap => {
                if let Some((from, _)) = &mut self.from {
                    std::mem::swap(from, at);
                }
            }
            // With nothing selected the line is, as three clicks select it,
            // until the caret is put away.
            Mark::Yank => {
                if self.from.is_none() {
                    let line = terminal.grid_ref(Point::Screen(here))?;
                    let line = terminal.select_line(SelectLineOptions::new(line))?;
                    terminal.set_selection(line.as_ref())?;
                }
                return Ok(());
            }
            Mark::End => {
                *self = Self::default();
                terminal.set_selection(None)?;
                return Ok(());
            }
        }
        self.show(terminal)
    }

    /// Go with the terminal to the screen it shows, when a program has it
    /// show the other: the caret begins again there, as it began here.
    pub fn follow(&mut self, terminal: &mut Terminal<'static, '_>) -> Result<()> {
        if self.at.is_none() || terminal.active_screen()? == self.screen {
            return Ok(());
        }
        *self = Self::default();
        self.mark(terminal, Mark::Begin)
    }

    /// Take up what has been selected otherwise, by the pointer or as a match
    /// found: the caret goes to its end, and stretches it from its start.
    /// With nothing selected, it stretches nothing, and goes to where the
    /// pointer was `pressed` if it was.
    pub fn adopt(
        &mut self,
        terminal: &mut Terminal<'static, '_>,
        pressed: Option<[f32; 2]>,
    ) -> Result<()> {
        self.follow(terminal)?;
        if self.at.is_none() {
            return Ok(());
        }
        let selected = terminal.selection()?;
        // What the caret itself selected it keeps as it is, in its shape: a
        // selection says only whether it is a block.
        if let Some(selected) = &selected
            && let Some((start, end, block)) = self.ends(terminal)?
        {
            let end_of = |at| terminal.point_from_grid_ref(&at, Space::Screen);
            let ends = end_of(selected.start())?.zip(end_of(selected.end())?);
            let same = ends == Some((start, end)) || ends == Some((end, start));
            if same && selected.is_rectangle() == block {
                return Ok(());
            }
        }
        let ends = match selected {
            Some(selected) => {
                let end = |at| terminal.point_from_grid_ref(&at, Space::Screen);
                let shape = if selected.is_rectangle() { Shape::Block } else { Shape::Chars };
                end(selected.start())?.zip(end(selected.end())?).map(|ends| (ends, shape))
            }
            None => None,
        };
        let to = match ends {
            Some(((start, end), shape)) => {
                self.from = Some((terminal.track_grid_ref(Point::Screen(start))?, shape));
                end
            }
            None => {
                self.from = None;
                let Some([x, y]) = pressed else {
                    return Ok(());
                };
                let bar = terminal.scrollbar()?;
                let last = |at: f32, cells: u64| (at.max(0.0) as u64).min(cells.saturating_sub(1));
                let y = bar.offset + last(y, bar.len);
                let to = Coordinate { x: last(x, terminal.cols()?.into()) as u16, y: y as u32 };
                Cells::of(terminal, false)?.glyph(to)?
            }
        };
        if let Some(at) = &mut self.at {
            at.set(terminal, Point::Screen(to))?;
        }
        Ok(())
    }

    /// Bring the caret into view, and select from where it stretches from.
    fn show(&self, terminal: &mut Terminal<'static, '_>) -> Result<()> {
        let Some(at) = &self.at else {
            return Ok(());
        };
        let here = point(at);
        let bar = terminal.scrollbar()?;
        let y = u64::from(here.y);
        if y < bar.offset {
            terminal.scroll_viewport(ScrollViewport::Row(y as usize));
        } else if y >= bar.offset + bar.len {
            terminal.scroll_viewport(ScrollViewport::Row((y + 1 - bar.len) as usize));
        }
        let Some((start, end, block)) = self.ends(terminal)? else {
            terminal.set_selection(None)?;
            return Ok(());
        };
        let (start, end) = (Point::Screen(start), Point::Screen(end));
        let selected = Selection::new(terminal.grid_ref(start)?, terminal.grid_ref(end)?, block);
        terminal.set_selection(Some(&selected))?;
        Ok(())
    }

    /// The ends of what the caret selects, on the screen, and whether it is a
    /// block: None while it stretches nothing.
    fn ends(&self, terminal: &Terminal) -> Result<Option<(Coordinate, Coordinate, bool)>> {
        let (Some(at), Some((from, shape))) = (&self.at, &self.from) else {
            return Ok(None);
        };
        let (mut start, mut end) = (point(from), point(at));
        if *shape == Shape::Lines {
            let last = terminal.cols()?.saturating_sub(1);
            let (top, bottom) = (start.y.min(end.y), start.y.max(end.y));
            (start, end) = (Coordinate { x: 0, y: top }, Coordinate { x: last, y: bottom });
        }
        Ok(Some((start, end, *shape == Shape::Block)))
    }
}

/// Where a tracked place is on the screen. One whose history has been let go
/// of is at the top.
fn point(at: &TrackedGridRef) -> Coordinate {
    at.point(Space::Screen).ok().flatten().unwrap_or(Coordinate { x: 0, y: 0 })
}

/// Where the caret begins, by `Mark::Begin`.
fn start(terminal: &Terminal) -> Result<Coordinate> {
    let bar = terminal.scrollbar()?;
    let bottom = bar.offset + bar.len >= bar.total;
    // The terminal's cursor is in the last rows of all, which are in view only
    // at the bottom.
    let rows = u64::from(terminal.rows()?);
    let (x, y) = match bottom {
        true => {
            (terminal.cursor_x()?, bar.total.saturating_sub(rows) + u64::from(terminal.cursor_y()?))
        }
        false => (0, (bar.offset + bar.len).saturating_sub(1)),
    };
    Ok(Coordinate { x, y: y as u32 })
}

/// What a cell holds, as words go.
#[derive(Clone, Copy, PartialEq)]
enum Class {
    Blank,
    Word,
    Other,
}

/// The cells the caret moves through, a screen's columns wide and all its
/// rows down.
struct Cells<'a, 'b> {
    terminal: &'a Terminal<'static, 'b>,
    cols: u16,
    total: u32,
    long: bool,
}

impl Cells<'_, '_> {
    fn of<'a, 'b>(terminal: &'a Terminal<'static, 'b>, long: bool) -> Result<Cells<'a, 'b>> {
        let total = terminal.scrollbar()?.total.max(1) as u32;
        Ok(Cells { terminal, cols: terminal.cols()?.max(1), total, long })
    }

    fn class(&self, at: Coordinate) -> Result<Class> {
        let cell = self.terminal.grid_ref(Point::Screen(at))?.cell()?;
        if !cell.has_text()? {
            return Ok(Class::Blank);
        }
        Ok(match char::from_u32(cell.codepoint()?).unwrap_or(' ') {
            c if c.is_whitespace() => Class::Blank,
            c if self.long || c.is_alphanumeric() || c == '_' => Class::Word,
            _ => Class::Other,
        })
    }

    /// The spare half of a wide glyph, or the cell a wide glyph left empty
    /// where it wrapped onto the next row. The caret never rests on either.
    fn spare(&self, at: Coordinate) -> Result<bool> {
        let wide = self.terminal.grid_ref(Point::Screen(at))?.cell()?.wide()?;
        Ok(matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead))
    }

    /// Whether a row runs on into the next, written as one line.
    fn wraps(&self, y: u32) -> Result<bool> {
        let row = self.terminal.grid_ref(Point::Screen(Coordinate { x: 0, y }))?.row()?;
        Ok(row.is_wrapped()?)
    }

    /// The cell after one, or before it, reading on from row to row, and
    /// whether a line ends between them. None at either end of it all.
    fn step(&self, mut at: Coordinate, forward: bool) -> Result<Option<(Coordinate, bool)>> {
        let mut ended = false;
        loop {
            let last = self.cols - 1;
            at = match (forward, at.x) {
                (true, x) if x < last => Coordinate { x: x + 1, ..at },
                (true, _) if at.y + 1 < self.total => {
                    ended |= !self.wraps(at.y)?;
                    Coordinate { x: 0, y: at.y + 1 }
                }
                (false, x) if x > 0 => Coordinate { x: x - 1, ..at },
                (false, _) if at.y > 0 => {
                    ended |= !self.wraps(at.y - 1)?;
                    Coordinate { x: last, y: at.y - 1 }
                }
                _ => return Ok(None),
            };
            if !self.spare(at)? {
                return Ok(Some((at, ended)));
            }
        }
    }

    /// Vim's `w`, and its `b` going back.
    fn word_start(&self, from: Coordinate, forward: bool) -> Result<Coordinate> {
        if !forward {
            return self.word_end(from, false);
        }
        let (first, mut at) = (self.class(from)?, from);
        // Past the rest of the word the caret is in, if it is in one.
        loop {
            let Some((next, ended)) = self.step(at, true)? else {
                return Ok(at);
            };
            at = next;
            if ended || first == Class::Blank || self.class(at)? != first {
                break;
            }
        }
        // And past the blanks after it.
        while self.class(at)? == Class::Blank {
            let Some((next, _)) = self.step(at, true)? else {
                break;
            };
            at = next;
        }
        Ok(at)
    }

    /// Vim's `e`, and going back, its `b`: on at least a cell, past blanks,
    /// and on to the far end of the word come to.
    fn word_end(&self, from: Coordinate, forward: bool) -> Result<Coordinate> {
        let Some((mut at, _)) = self.step(from, forward)? else {
            return Ok(from);
        };
        while self.class(at)? == Class::Blank {
            let Some((next, _)) = self.step(at, forward)? else {
                return Ok(at);
            };
            at = next;
        }
        let word = self.class(at)?;
        while let Some((next, false)) = self.step(at, forward)? {
            if self.class(next)? != word {
                break;
            }
            at = next;
        }
        Ok(at)
    }

    /// The first column of a row that is not blank, or the last: the first
    /// column of all where every one is blank.
    fn text(&self, y: u32, first: bool) -> Result<u16> {
        Ok(self.written(y, first)?.unwrap_or(0))
    }

    /// The same, or None where every column is blank.
    fn written(&self, y: u32, first: bool) -> Result<Option<u16>> {
        for i in 0..self.cols {
            let x = if first { i } else { self.cols - 1 - i };
            if self.class(Coordinate { x, y })? != Class::Blank {
                return Ok(Some(x));
            }
        }
        Ok(None)
    }

    /// The start of a row's text, as Vim goes to after a move of many lines.
    fn line(&self, y: u32) -> Result<Coordinate> {
        let y = y.min(self.total - 1);
        Ok(Coordinate { x: self.text(y, true)?, y })
    }

    /// A column, or the glyph whose spare half it is.
    fn glyph(&self, at: Coordinate) -> Result<Coordinate> {
        Ok(match at.x > 0 && self.spare(at)? {
            true => Coordinate { x: at.x - 1, ..at },
            false => at,
        })
    }
}

/// Where a motion takes the caret from where it is, on the screen. The view
/// moves first, for a motion that moves it.
fn moved(
    terminal: &mut Terminal<'static, '_>,
    at: Coordinate,
    motion: Motion,
) -> Result<Coordinate> {
    let bar = terminal.scrollbar()?;
    if let Motion::Halves(halves) = motion {
        let by = halves as i64 * (bar.len / 2).max(1) as i64;
        terminal.scroll_viewport(ScrollViewport::Delta(by as isize));
        let y = (i64::from(at.y) + by).clamp(0, bar.total.saturating_sub(1) as i64);
        return Cells::of(terminal, false)?.glyph(Coordinate { y: y as u32, ..at });
    }
    let long = matches!(
        motion,
        Motion::WordNext { long: true }
            | Motion::WordBack { long: true }
            | Motion::WordEnd { long: true }
    );
    let cells = Cells::of(terminal, long)?;
    let (last, top) = (cells.cols - 1, bar.offset as u32);
    let view = bar.len.max(1) as u32;
    let to = match motion {
        // Across a wide glyph, which is one place to be.
        Motion::Left => match cells.step(at, false)? {
            Some((to, _)) if to.y == at.y => to,
            _ => at,
        },
        Motion::Right => match cells.step(at, true)? {
            Some((to, _)) if to.y == at.y => to,
            _ => at,
        },
        Motion::Up => cells.glyph(Coordinate { y: at.y.saturating_sub(1), ..at })?,
        Motion::Down => cells.glyph(Coordinate { y: (at.y + 1).min(cells.total - 1), ..at })?,
        Motion::WordNext { .. } => cells.word_start(at, true)?,
        Motion::WordBack { .. } => cells.word_start(at, false)?,
        Motion::WordEnd { .. } => cells.word_end(at, true)?,
        Motion::LineStart => Coordinate { x: 0, ..at },
        Motion::LineText => Coordinate { x: cells.text(at.y, true)?, ..at },
        Motion::LineEnd => Coordinate { x: cells.text(at.y, false)?, ..at },
        Motion::Top => cells.line(0)?,
        // Below the last line written there may be rows yet to be written to,
        // as many as the screen has.
        Motion::Bottom => {
            let rows = u32::from(terminal.rows()?);
            let floor = cells.total.saturating_sub(rows);
            let mut y = cells.total - 1;
            while y > floor && cells.written(y, false)?.is_none() {
                y -= 1;
            }
            cells.line(y)?
        }
        Motion::ViewTop => cells.line(top)?,
        Motion::ViewMiddle => cells.line(top + (view - 1) / 2)?,
        Motion::ViewBottom => cells.line(top + view - 1)?,
        Motion::Halves(_) => unreachable!("moved above"),
    };
    Ok(Coordinate { x: to.x.min(last), ..to })
}

#[cfg(test)]
#[path = "../../tests/unit/terminal_caret.rs"]
mod tests;
