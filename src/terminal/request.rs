//! What the UI asks of a pane's worker: to move its view, and about its selection
//! and the text looked for in it. And the selected text the worker hands back.
use anyhow::Result;
use libghostty_vt::{Terminal, terminal::ScrollViewport};

use super::{
    Shared,
    caret::{Caret, Mark},
    find::Finding,
    select::{Selecting, text},
};

/// What the UI asks of a pane's worker, which does it in the order asked. A
/// place is given in cells from the grid's top left corner, and may lie outside
/// the grid.
pub enum Request {
    /// The button that selects goes down: once to let go of what is selected,
    /// twice for a word and three times for a line. To `extend` is to have
    /// what is selected reach to here instead.
    Press {
        at: [f32; 2],
        extend: bool,
    },
    /// The pointer moves with that button down. A `block` is selected by columns.
    /// Held past the top or the bottom, the view follows it.
    Drag {
        at: [f32; 2],
        block: bool,
    },
    /// The button is let go, and what is selected is the desktop's selection.
    Release,
    /// Select all there is, history too.
    SelectAll,
    Deselect,
    /// Hand over what is selected, for the clipboard.
    Copy,
    /// Look for this text and select the match nearest the end, or with no text
    /// stop looking.
    Find(String),
    /// Select the match before the one selected, which is the `older`, or the
    /// one after it.
    Step {
        older: bool,
    },
    /// Move the view through the pane's history.
    Scroll(Scroll),
    /// Show the caret that selects by the keyboard, move it, or put it away.
    Mark(Mark),
}

/// A move through a pane's history. Negative is up.
pub enum Scroll {
    Lines(i32),
    /// A screenful at a time.
    Pages(i32),
    /// From one shell prompt to another, where the shell marks them (OSC 133).
    Prompts(i32),
    /// To where this row is the view's first, counted from the first of all
    /// there is.
    Row(usize),
    Bottom,
}

/// Selected text a worker hands back: for the clipboard, or as the desktop's
/// selection, which a middle click pastes.
#[derive(Debug, PartialEq)]
pub enum Copied {
    Clipboard(String),
    Selection(String),
}

/// Do what the handle asked, in the order asked. True when a frame may come of
/// it: every key typed asks for the bottom, where the view mostly is already.
pub fn heed(
    shared: &Shared,
    terminal: &mut Terminal<'static, '_>,
    selecting: &mut Selecting,
    finding: &mut Finding,
    caret: &mut Caret,
) -> Result<bool> {
    let asks = std::mem::take(&mut *shared.asks.lock().unwrap());
    if asks.is_empty() {
        return Ok(false);
    }
    let before = terminal.scrollbar()?.offset;
    let mut owed = false;
    for request in asks {
        owed |= !matches!(request, Request::Scroll(_));
        // What is selected once it is done is handed back, where it is wanted.
        let wanted: Option<fn(String) -> Copied> = match request {
            Request::Release | Request::SelectAll => Some(Copied::Selection),
            Request::Copy | Request::Mark(Mark::Yank) => Some(Copied::Clipboard),
            _ => None,
        };
        // What is selected otherwise the caret takes up, and a press with
        // nothing selected moves it.
        let (adopts, pressed) = match request {
            Request::Press { at, .. } => (true, Some(at)),
            Request::Drag { .. } | Request::Release | Request::SelectAll | Request::Deselect => {
                (true, None)
            }
            Request::Find(_) | Request::Step { .. } => (true, None),
            _ => (false, None),
        };
        match request {
            Request::Press { at, extend } => selecting.press(terminal, at, extend)?,
            Request::Drag { at, block } => selecting.drag(terminal, at, block)?,
            Request::Release => selecting.release(terminal)?,
            Request::SelectAll => {
                let all = terminal.select_all()?;
                terminal.set_selection(all.as_ref())?;
            }
            Request::Deselect => drop(terminal.set_selection(None)?),
            Request::Copy => {}
            Request::Find(text) => finding.look(terminal, &text)?,
            Request::Step { older } => finding.step(terminal, older)?,
            Request::Scroll(to) => {
                let rows = terminal.rows()? as isize;
                terminal.scroll_viewport(match to {
                    Scroll::Lines(lines) => ScrollViewport::Delta(lines as isize),
                    Scroll::Pages(pages) => ScrollViewport::Delta(pages as isize * rows),
                    Scroll::Prompts(prompts) => ScrollViewport::DeltaPrompt(prompts as isize),
                    Scroll::Row(row) => ScrollViewport::Row(row),
                    Scroll::Bottom => ScrollViewport::Bottom,
                });
            }
            Request::Mark(mark) => caret.mark(terminal, mark)?,
        }
        if adopts {
            caret.adopt(terminal, pressed)?;
        }
        if let Some(wanted) = wanted
            && let Some(text) = text(terminal)?
        {
            shared.copied.lock().unwrap().push(wanted(text));
            (shared.repaint)();
        }
    }
    Ok(owed || terminal.scrollbar()?.offset != before)
}
