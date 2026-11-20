use super::*;
use crate::terminal::{
    Copied, Request,
    select::{Selecting, text},
    tests::{encode, lined, render, row, terminal, until, worker, write},
};

/// Where the caret is in view.
fn at(caret: &Caret) -> (u16, u16) {
    caret.viewed().unwrap().unwrap().unwrap()
}

/// Move the caret, and say where to.
fn go(caret: &mut Caret, terminal: &mut Terminal<'static, '_>, motion: Motion) -> (u16, u16) {
    caret.mark(terminal, Mark::Move(motion)).unwrap();
    at(caret)
}

/// What copying would copy.
fn copied(terminal: &Terminal) -> Option<String> {
    text(terminal).unwrap()
}

#[test]
fn the_caret_moves_by_words_and_lines_as_vims_does() {
    let mut terminal = terminal(30, 4);
    terminal.vt_write(b"foo.bar baz  qux\r\nnext-line here\r\n   indented");
    let mut caret = Caret::default();
    assert_eq!(caret.viewed().unwrap(), None);
    // It begins at the terminal's cursor.
    caret.mark(&mut terminal, Mark::Begin).unwrap();
    assert_eq!(at(&caret), (11, 2));
    let mut go = |motion| go(&mut caret, &mut terminal, motion);
    assert_eq!(go(Motion::Top), (0, 0));
    // Letters and punctuation are words apart, as they are to Vim's `w`.
    let next = Motion::WordNext { long: false };
    let steps: Vec<_> = (0..5).map(|_| go(next)).collect();
    assert_eq!(steps, [(3, 0), (4, 0), (8, 0), (13, 0), (0, 1)]);
    let back = Motion::WordBack { long: false };
    let steps: Vec<_> = (0..5).map(|_| go(back)).collect();
    assert_eq!(steps, [(13, 0), (8, 0), (4, 0), (3, 0), (0, 0)]);
    let end = Motion::WordEnd { long: false };
    let steps: Vec<_> = (0..4).map(|_| go(end)).collect();
    assert_eq!(steps, [(2, 0), (3, 0), (6, 0), (10, 0)]);
    // Blanks alone part long words, as Vim's WORDs.
    assert_eq!(go(Motion::LineStart), (0, 0));
    assert_eq!(go(Motion::WordNext { long: true }), (8, 0));
    assert_eq!(go(Motion::WordEnd { long: true }), (10, 0));
    assert_eq!(go(Motion::WordBack { long: true }), (8, 0));
    assert_eq!(go(Motion::WordBack { long: true }), (0, 0));
    // A line's ends, and where its text begins.
    assert_eq!(go(Motion::LineEnd), (15, 0));
    assert_eq!(go(Motion::Down), (15, 1));
    assert_eq!(go(Motion::Down), (15, 2));
    assert_eq!(go(Motion::LineText), (3, 2));
    assert_eq!(go(Motion::LineStart), (0, 2));
    // Along a line and no further, and up and down no further than there is.
    assert_eq!(go(Motion::Left), (0, 2));
    assert_eq!(go(Motion::Right), (1, 2));
    assert_eq!(go(Motion::Top), (0, 0));
    assert_eq!(go(Motion::Up), (0, 0));
    // The last line written, with a row yet to be written below it.
    assert_eq!(go(Motion::Bottom), (3, 2));
    assert_eq!(go(Motion::Down), (3, 3));
    assert_eq!(go(Motion::Down), (3, 3));
    assert_eq!(go(Motion::ViewTop), (0, 0));
    assert_eq!(go(Motion::ViewMiddle), (0, 1));
    assert_eq!(go(Motion::ViewBottom), (0, 3));
}

#[test]
fn a_wide_glyph_is_one_place_for_the_caret() {
    let mut terminal = terminal(20, 2);
    terminal.vt_write("a界b 界界\r\nabcdefgh".as_bytes());
    let mut caret = Caret::default();
    caret.mark(&mut terminal, Mark::Begin).unwrap();
    let mut go = |motion| go(&mut caret, &mut terminal, motion);
    assert_eq!(go(Motion::Top), (0, 0));
    assert_eq!(go(Motion::Right), (1, 0));
    assert_eq!(go(Motion::Right), (3, 0));
    assert_eq!(go(Motion::Left), (1, 0));
    assert_eq!(go(Motion::LineEnd), (7, 0));
    assert_eq!(go(Motion::Down), (7, 1));
    // Up onto the spare half of one, it is at the glyph.
    assert_eq!(go(Motion::Left), (6, 1));
    assert_eq!(go(Motion::Up), (5, 0));
    // Letters of any script are a word's.
    assert_eq!(go(Motion::WordBack { long: false }), (0, 0));
}

#[test]
fn it_selects_from_where_it_was_in_each_shape_and_copies() {
    let mut terminal = terminal(30, 3);
    terminal.vt_write(b"foo.bar baz\r\nnext-line here");
    let mut caret = Caret::default();
    macro_rules! mark {
        ($mark:expr) => {
            caret.mark(&mut terminal, $mark).unwrap()
        };
    }
    mark!(Mark::Begin);
    // Nothing is selected until it is stretched.
    mark!(Mark::Move(Motion::Top));
    mark!(Mark::Move(Motion::WordNext { long: false }));
    mark!(Mark::Move(Motion::WordNext { long: false }));
    assert_eq!(copied(&terminal), None);
    mark!(Mark::Stretch(Shape::Chars));
    mark!(Mark::Move(Motion::WordEnd { long: false }));
    assert_eq!(copied(&terminal).as_deref(), Some("bar"));
    // From its other end, back the other way.
    mark!(Mark::Swap);
    mark!(Mark::Move(Motion::LineStart));
    assert_eq!(copied(&terminal).as_deref(), Some("foo.bar"));
    // Whole lines, and the columns of a block.
    mark!(Mark::Move(Motion::Down));
    mark!(Mark::Stretch(Shape::Lines));
    assert_eq!(copied(&terminal).as_deref(), Some("foo.bar baz\nnext-line here"));
    mark!(Mark::Stretch(Shape::Block));
    assert_eq!(copied(&terminal).as_deref(), Some("foo.bar\nnext-li"));
    let shown = render(&terminal).selected.clone();
    assert_eq!(shown, [Some((0, 7)), Some((0, 7)), None]);
    // The same shape again selects nothing.
    mark!(Mark::Stretch(Shape::Block));
    assert_eq!(copied(&terminal), None);
    // With nothing selected, what is copied is the caret's line.
    mark!(Mark::Yank);
    assert_eq!(copied(&terminal).as_deref(), Some("next-line here"));
    mark!(Mark::End);
    assert_eq!(copied(&terminal), None);
    assert_eq!(caret.viewed().unwrap(), None);
}

#[test]
fn it_takes_up_what_the_pointer_selected_and_goes_where_it_pressed() {
    let mut terminal = terminal(20, 2);
    terminal.vt_write(b"one two three");
    let mut selecting = Selecting::new().unwrap();
    selecting.press(&terminal, [4.1, 0.5], false).unwrap();
    selecting.drag(&terminal, [6.9, 0.5], false).unwrap();
    selecting.release(&terminal).unwrap();
    let mut caret = Caret::default();
    caret.mark(&mut terminal, Mark::Begin).unwrap();
    assert_eq!(at(&caret), (6, 0));
    caret.mark(&mut terminal, Mark::Move(Motion::Right)).unwrap();
    caret.mark(&mut terminal, Mark::Move(Motion::WordEnd { long: false })).unwrap();
    assert_eq!(copied(&terminal).as_deref(), Some("two three"));
    // A press that selects nothing has it there, selecting nothing.
    selecting.press(&terminal, [2.5, 1.5], false).unwrap();
    selecting.release(&terminal).unwrap();
    caret.adopt(&mut terminal, Some([2.5, 1.5])).unwrap();
    assert_eq!((at(&caret), copied(&terminal)), ((2, 1), None));
}

#[test]
fn it_brings_history_into_view_as_it_goes_there() {
    let mut terminal = lined(3, 100);
    let mut caret = Caret::default();
    caret.mark(&mut terminal, Mark::Begin).unwrap();
    assert_eq!(at(&caret), (0, 2));
    macro_rules! go {
        ($motion:expr) => {
            go(&mut caret, &mut terminal, $motion)
        };
    }
    assert_eq!(go!(Motion::Top), (0, 0));
    assert_eq!(go!(Motion::Halves(1)), (0, 0));
    assert_eq!(go!(Motion::Down), (0, 1));
    assert_eq!(go!(Motion::Halves(2)), (0, 1));
    assert_eq!(row(&render(&terminal), 1), "line 4");
    assert_eq!(go!(Motion::Bottom), (0, 2));
    assert_eq!(row(&render(&terminal), 2), "line 99");
    assert_eq!(go!(Motion::Top), (0, 0));
    assert_eq!(row(&render(&terminal), 0), "line 0");
}

#[test]
fn a_worker_draws_the_caret_for_the_cursor_and_copies_from_it() {
    let (handle, wakes) = worker(encode(&mut terminal(20, 2)));
    write(&handle, b"hello world");
    until(&wakes, || handle.latest().is_some_and(|frame| frame.cursor == Some((11, 0))));
    assert!(!handle.latest().unwrap().caret);
    let mark = |mark| handle.ask(Request::Mark(mark));
    mark(Mark::Begin);
    mark(Mark::Move(Motion::LineStart));
    until(&wakes, || {
        handle.latest().is_some_and(|frame| frame.caret && frame.cursor == Some((0, 0)))
    });
    let frame = handle.latest().unwrap();
    assert!(!frame.cursor_blink);
    mark(Mark::Stretch(Shape::Chars));
    mark(Mark::Move(Motion::WordEnd { long: false }));
    mark(Mark::Yank);
    let mut copies = Vec::new();
    until(&wakes, || {
        copies.extend(handle.copied());
        !copies.is_empty()
    });
    assert_eq!(copies, [Copied::Clipboard("hello".into())]);
    // Put away, the program's cursor is back, and nothing is selected.
    mark(Mark::End);
    until(&wakes, || handle.latest().is_some_and(|frame| !frame.caret));
    let frame = handle.latest().unwrap();
    assert_eq!((frame.cursor, frame.selected.is_empty()), (Some((11, 0)), true));
}

#[test]
fn it_begins_again_on_the_screen_a_program_switches_to() {
    let mut terminal = lined(3, 100);
    let mut caret = Caret::default();
    caret.mark(&mut terminal, Mark::Begin).unwrap();
    caret.mark(&mut terminal, Mark::Move(Motion::Top)).unwrap();
    caret.mark(&mut terminal, Mark::Stretch(Shape::Chars)).unwrap();
    // A program takes the whole screen, which keeps no history.
    terminal.vt_write(b"\x1b[?1049h\x1b[Hfull screen");
    caret.follow(&mut terminal).unwrap();
    assert_eq!(at(&caret), (11, 0));
    assert_eq!(copied(&terminal), None);
    for motion in [Motion::Top, Motion::WordNext { long: false }, Motion::Up, Motion::Bottom] {
        caret.mark(&mut terminal, Mark::Move(motion)).unwrap();
    }
    assert_eq!(at(&caret), (0, 0));
    // And back again, as it is asked to move.
    terminal.vt_write(b"\x1b[?1049l");
    caret.mark(&mut terminal, Mark::Move(Motion::Top)).unwrap();
    assert_eq!(row(&render(&terminal), 0), "line 0");
}

#[test]
fn what_it_selected_itself_it_keeps_in_its_shape() {
    let mut terminal = terminal(20, 3);
    terminal.vt_write(b"one two\r\nthree four\r\nfive");
    let mut caret = Caret::default();
    caret.mark(&mut terminal, Mark::Begin).unwrap();
    caret.mark(&mut terminal, Mark::Stretch(Shape::Lines)).unwrap();
    caret.mark(&mut terminal, Mark::Move(Motion::Up)).unwrap();
    let before = (at(&caret), copied(&terminal));
    // As a search that finds more does, or a step to a match on show.
    caret.adopt(&mut terminal, None).unwrap();
    assert_eq!((at(&caret), copied(&terminal)), before);
    caret.mark(&mut terminal, Mark::Move(Motion::Up)).unwrap();
    assert_eq!(copied(&terminal).as_deref(), Some("one two\nthree four\nfive"));
}

#[test]
fn a_press_on_half_a_wide_glyph_is_on_the_glyph() {
    let mut terminal = terminal(20, 2);
    terminal.vt_write("a界b".as_bytes());
    let mut caret = Caret::default();
    caret.mark(&mut terminal, Mark::Begin).unwrap();
    caret.adopt(&mut terminal, Some([2.5, 0.5])).unwrap();
    assert_eq!(at(&caret), (1, 0));
}
