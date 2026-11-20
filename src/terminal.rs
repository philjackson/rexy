//! Ghostty belongs to one worker per pane. The UI only borrows immutable frames,
//! and asks the worker for what is to be selected or found.
//!
//! A pane starts with a streamed Ghostty snapshot, then carries VT output with
//! nonce-tagged Rex metadata. Neither socket reads nor UI callbacks decode it.
mod caret;
mod find;
mod request;
mod select;

use anyhow::{Context, Result, anyhow, ensure};
use libghostty_vt::{
    RenderState, Terminal,
    color::{Palette, Rgb},
    kitty::graphics::{self, ImageFormat},
    mode::Mode,
    point::{Coordinate, Point},
    render::{
        CellIterator, Colors, CursorVisualStyle, Dirty, ImageIterator, ImageLayer, ImageProtocol,
        RowId, RowIteration, RowIterator, Snapshot,
    },
    screen::{CellContentTag, CellWide, Screen},
    snapshot,
    style::{Style, StyleColor},
    terminal::{CompressionActivity, CompressionMode, CompressionResult},
};
use serde_json::Value;
use std::{
    cell::RefCell,
    collections::HashMap,
    io::{self, Read},
    rc::Rc,
    sync::{
        Arc, Mutex, Once, RwLock,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use caret::Caret;
pub use caret::{Mark, Motion, Shape};
use find::Finding;
pub use request::{Copied, Request, Scroll};
use select::Selecting;

const CHUNK_BYTES: usize = 64 * 1024;
const QUEUE_CHUNKS: usize = 16;
const MAX_SNAPSHOT_BYTES: u64 = 256 * 1024 * 1024;
const MAX_APC_BYTES: usize = 1024 * 1024;
const SCROLLBACK_BYTES: usize = 32 * 1024 * 1024;
/// What the pictures of a pane may take in memory, on each of its two screens.
/// The oldest go when more arrive.
const PICTURE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_LINK_BYTES: usize = 2048;
const FRAME_INTERVAL: Duration = Duration::from_nanos(1_000_000_000 / 60);
const RENDER_HOLD_LIMIT: Duration = Duration::from_secs(1);
/// How long a terminal's history is left alone before it is packed.
const PACK_AFTER: Duration = Duration::from_secs(2);

pub type Notify = Arc<dyn Fn() + Send + Sync>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CursorShape {
    Block,
    Bar,
    Underline,
    Hollow,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cell {
    /// Where its text begins in its row's, which is where the cell before ends.
    at: u32,
    /// Which of its row's looks is its own. Cells side by side share one.
    pub look: u16,
    /// The trailing cell of a wide glyph: do not draw text.
    pub spacer_tail: bool,
    /// An unused last column when a wide glyph wraps to the following row.
    pub spacer_head: bool,
}

/// How a run of cells is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Look {
    /// Effective colors: reverse-video has already been applied.
    pub fg: u32,
    pub bg: u32,
    pub underline_color: Option<u32>,
    pub style: Style,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// What its cells show, one after another: most of them a character, a blank
    /// one nothing, a few a cluster of several.
    text: String,
    looks: Vec<Look>,
    pub wrapped: bool,
    /// The links its programs have marked: columns from one up to another, and
    /// where they lead.
    pub links: Vec<(usize, usize, Arc<str>)>,
}

impl Row {
    /// What the columns from one up to another show.
    pub fn text(&self, start: usize, end: usize) -> &str {
        let at = |x: usize| self.cells.get(x).map_or(self.text.len(), |c| c.at as usize);
        &self.text[at(start)..at(end)]
    }

    pub fn look(&self, cell: &Cell) -> &Look {
        &self.looks[usize::from(cell.look)]
    }
}

/// A picture a program sent, as pixels of four bytes each: red, green, blue and
/// how opaque.
#[derive(Debug)]
pub struct Picture {
    /// Ghostty's name for these pixels. No others are ever given it, in this
    /// terminal or another.
    pub id: u64,
    /// How many pixels wide and tall.
    pub size: [usize; 2],
    pub rgba: Vec<u8>,
}

/// A picture where a frame shows it.
#[derive(Clone, Debug)]
pub struct Placed {
    pub picture: Arc<Picture>,
    /// It lies under the text, not over it.
    pub under: bool,
    /// Its left, its top, its width and its height in cells and parts of them,
    /// from the frame's top left. It may begin before the frame and end after.
    pub at: [f32; 4],
    /// The part of the picture shown there: its left, top, right and bottom,
    /// each as a fraction of the whole.
    pub part: [f32; 4],
}

#[derive(Clone, Debug)]
pub struct Frame {
    pub cols: u16,
    pub rows: u16,
    pub lines: Vec<Arc<Row>>,
    pub fg: u32,
    pub bg: u32,
    pub cursor: Option<(u16, u16)>,
    pub cursor_color: u32,
    /// The theme's red, green, yellow and blue.
    pub ansi: [u32; 4],
    pub cursor_shape: CursorShape,
    /// The cursor blinks: its program asked for that, or asked nothing of it.
    pub cursor_blink: bool,
    pub mouse_tracking: bool,
    /// The alternate screen is showing, which keeps no history to scroll through.
    pub alternate_screen: bool,
    /// The program would have the wheel send arrow keys there (DEC mode 1007).
    pub alternate_scroll: bool,
    /// Distance in rows from the active terminal, zero when following output.
    /// Scrolled back, it is as of this frame: history that grows under a view
    /// it leaves as it was draws no new one. So is `history`, the rows there
    /// are above the screen in all.
    pub scroll_offset: usize,
    pub history: usize,
    /// What is selected of each row, as columns from one up to another. Empty
    /// when nothing is selected anywhere, in view or out of it.
    pub selected: Vec<Option<(u16, u16)>>,
    /// While text is looked for: which of its matches is selected, the newest
    /// being the first, and how many have been found.
    pub found: Option<(usize, usize)>,
    /// The cursor is the caret that selects by the keyboard, not the program's.
    pub caret: bool,
    /// The pictures on show, in the order they are drawn.
    pub pictures: Arc<[Placed]>,
    /// What of a word is out of sight where the rows shown begin or end in the
    /// middle of one: the part of it above the first row, and the part below
    /// the last, on a line that wraps there. An address may be written across
    /// such an edge. None where there is more of it than an address may be long.
    pub beyond: [Option<String>; 2],
}

impl Frame {
    /// The text of every row on show, a line to each but where a row wraps into
    /// the next. For tests: what the user copies is read from the terminal.
    pub fn text(&self) -> String {
        let mut text = String::new();
        for row in &self.lines {
            for (x, cell) in row.cells.iter().enumerate() {
                match row.text(x, x + 1) {
                    _ if cell.spacer_tail || cell.spacer_head => {}
                    "" => text.push(' '),
                    shown => text.push_str(shown),
                }
            }
            if !row.wrapped {
                text.truncate(text.trim_end_matches(' ').len());
                text.push('\n');
            }
        }
        text.truncate(text.strip_suffix('\n').unwrap_or(&text).len());
        text
    }
}

enum Command {
    Bytes(Vec<u8>),
    Wake,
}

struct Shared {
    frame: RwLock<Option<Arc<Frame>>>,
    error: Mutex<Option<String>>,
    asks: Mutex<Vec<Request>>,
    copied: Mutex<Vec<Copied>>,
    repaint: Notify,
    /// Its pane is on show. One that is not draws nothing but its first frame:
    /// what it would draw meanwhile nobody sees.
    shown: AtomicBool,
}

/// Dropping the last handle disconnects the bounded queue and wakes the worker,
/// including a worker blocked in snapshot decoding. No worker retains a sender.
#[derive(Clone)]
pub struct TerminalHandle {
    input: flume::Sender<Command>,
    shared: Arc<Shared>,
}

impl TerminalHandle {
    pub fn new(nonce: u64, repaint: Notify, shown: bool) -> Result<Self> {
        // Ghostty decodes no PNG of its own, and is told how once, for every
        // terminal, before the first of them has a worker.
        static PNG: Once = Once::new();
        PNG.call_once(|| {
            let decoder = Box::new(graphics::RustPngDecoder::new());
            // SAFETY: only a terminal's worker and what a window is told of one
            // call into Ghostty, and until this returns there is no terminal.
            let _ = unsafe { graphics::set_png_decoder(Some(decoder)) };
        });
        let (input, receiver) = flume::bounded(QUEUE_CHUNKS);
        let shared = Arc::new(Shared {
            frame: RwLock::new(None),
            error: Mutex::new(None),
            asks: Mutex::default(),
            copied: Mutex::default(),
            repaint,
            shown: AtomicBool::new(shown),
        });
        let worker_shared = shared.clone();
        thread::Builder::new()
            .name("rex-terminal".into())
            .spawn(move || {
                if let Err(error) = run_worker(receiver, &worker_shared, nonce) {
                    *worker_shared.error.lock().unwrap() = Some(format!("{error:#}"));
                    (worker_shared.repaint)();
                }
            })
            .context("starting terminal worker")?;
        Ok(Self { input, shared })
    }

    /// Backpressure is intentional: terminal bytes must never be discarded.
    pub async fn feed(&self, bytes: Vec<u8>) -> Result<()> {
        if bytes.len() <= CHUNK_BYTES {
            return self.send(bytes).await;
        }
        for chunk in bytes.chunks(CHUNK_BYTES) {
            self.send(chunk.to_vec()).await?;
        }
        Ok(())
    }

    async fn send(&self, bytes: Vec<u8>) -> Result<()> {
        let sent = self.input.send_async(Command::Bytes(bytes)).await;
        sent.map_err(|_| anyhow!(self.status().unwrap_or_else(|| "terminal worker stopped".into())))
    }

    pub fn latest(&self) -> Option<Arc<Frame>> {
        self.shared.frame.read().unwrap().clone()
    }

    /// The error that stopped the worker, if any.
    pub fn status(&self) -> Option<String> {
        self.shared.error.lock().unwrap().clone()
    }

    /// Ask the worker to move its view, or about what is selected or looked for.
    /// What is asked waits apart from the terminal's output, which cannot hold it
    /// up. Nothing is said of a worker that has stopped: its pane says so.
    pub fn ask(&self, request: Request) {
        self.shared.asks.lock().unwrap().push(request);
        self.wake();
    }

    /// Say whether its pane is on show. One that comes on show draws what it has
    /// come to meanwhile.
    pub fn show(&self, shown: bool) {
        if !self.shared.shown.swap(shown, Ordering::Relaxed) && shown {
            self.wake();
        }
    }

    /// The selected text handed back since this was last asked.
    pub fn copied(&self) -> Vec<Copied> {
        std::mem::take(&mut self.shared.copied.lock().unwrap())
    }

    fn wake(&self) {
        // If the queue is full the worker is already awake. No bytes are lost.
        let _ = self.input.try_send(Command::Wake);
    }
}

/// Consumes at most one 64-KiB chunk at a time; unread VT bytes stay here after
/// the snapshot decoder reaches FINISH. Read runs only on the terminal thread.
struct Input {
    receiver: flume::Receiver<Command>,
    bytes: Vec<u8>,
    offset: usize,
}

impl Input {
    fn next(&mut self, timeout: Option<Duration>) -> Option<Command> {
        if self.offset < self.bytes.len() {
            let bytes = self.bytes.split_off(self.offset);
            self.bytes.clear();
            self.offset = 0;
            return Some(Command::Bytes(bytes));
        }
        match timeout {
            Some(timeout) => match self.receiver.recv_timeout(timeout) {
                Ok(command) => Some(command),
                Err(flume::RecvTimeoutError::Timeout) => Some(Command::Wake),
                Err(flume::RecvTimeoutError::Disconnected) => None,
            },
            None => self.receiver.recv().ok(),
        }
    }
}

impl Read for Input {
    fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
        if target.is_empty() {
            return Ok(0);
        }
        while self.offset == self.bytes.len() {
            match self.receiver.recv() {
                Ok(Command::Bytes(bytes)) => {
                    self.bytes = bytes;
                    self.offset = 0;
                }
                Ok(Command::Wake) => {}
                Err(_) => return Ok(0),
            }
        }
        let len = target.len().min(self.bytes.len() - self.offset);
        target[..len].copy_from_slice(&self.bytes[self.offset..self.offset + len]);
        self.offset += len;
        Ok(len)
    }
}

#[derive(Debug, PartialEq)]
enum Part {
    Output(Vec<u8>),
    Metadata(Value),
}

/// Only APCs bearing this stream's nonce are metadata. All other bytes retain
/// their exact order, including a partial escape prefix across socket frames.
struct ApcParser {
    prefix: Vec<u8>,
    pending: Vec<u8>,
}

impl ApcParser {
    fn new(nonce: u64) -> Self {
        Self { prefix: format!("\x1b_rex1;{nonce};").into_bytes(), pending: Vec::new() }
    }

    fn feed(&mut self, mut bytes: &[u8]) -> Result<Vec<Part>> {
        let mut parts = Vec::new();
        let mut output = Vec::new();
        while !bytes.is_empty() {
            if self.pending.is_empty() {
                let plain = bytes.iter().position(|&byte| byte == 0x1b).unwrap_or(bytes.len());
                output.extend_from_slice(&bytes[..plain]);
                bytes = &bytes[plain..];
                if bytes.is_empty() {
                    break;
                }
            }
            self.pending.push(bytes[0]);
            bytes = &bytes[1..];
            loop {
                if self.pending.starts_with(&self.prefix) {
                    ensure!(self.pending.len() <= MAX_APC_BYTES, "Rex metadata exceeds 1 MiB");
                    if self.pending.ends_with(b"\x1b\\") {
                        if !output.is_empty() {
                            parts.push(Part::Output(std::mem::take(&mut output)));
                        }
                        let json = &self.pending[self.prefix.len()..self.pending.len() - 2];
                        parts.push(Part::Metadata(
                            serde_json::from_slice(json).context("invalid Rex metadata")?,
                        ));
                        self.pending.clear();
                    }
                    break;
                }
                if self.prefix.starts_with(&self.pending) {
                    break;
                }
                output.push(self.pending.remove(0));
            }
        }
        if !output.is_empty() {
            parts.push(Part::Output(output));
        }
        Ok(parts)
    }
}

/// Restore a terminal from a snapshot. Ghostty checks the envelope and every
/// record as it streams in, and reads no further than the snapshot's end. It
/// bounds each record but not their sum, which is cut short here. A snapshot
/// begins with the screen, and the terminal is `seen` as soon as that is read:
/// its history follows, and the pixels of its pictures, which may be most of the
/// snapshot and long in coming. With the terminal, whether any of that came.
fn restore(
    input: impl Read,
    seen: impl FnOnce(&Terminal<'static, 'static>) -> Result<()>,
) -> Result<(Terminal<'static, 'static>, bool)> {
    let mut input = input.take(MAX_SNAPSHOT_BYTES);
    let mut decoder = snapshot::Decoder::new(&mut input)?;
    decoder.set_max_continuation_bytes(MAX_APC_BYTES)?;
    decoder.set_kitty_image_storage_limit(PICTURE_BYTES)?;
    // History is kept compressed, a page at a time as it arrives. A page opens
    // again when it is scrolled to.
    decoder.set_compress_history_enabled(true)?;
    let mut restoring = decoder.ready()?;
    configure(restoring.terminal_mut())?;
    seen(restoring.terminal())?;
    let mut more = false;
    while restoring.next()?.is_some() {
        more = true;
    }
    Ok((restoring.into_terminal(), more))
}

fn configure(terminal: &mut Terminal) -> Result<()> {
    // A program sends a picture's pixels in the escape itself. The protocol
    // would also have the terminal read them from a file, a temporary file it
    // then removes, or shared memory, by a name the program gives: here that
    // name would be read on this machine, for a program on another.
    terminal.set_kitty_image_storage_limit(PICTURE_BYTES)?;
    terminal.set_kitty_image_from_file_allowed(false)?;
    terminal.set_kitty_image_from_shared_mem_allowed(false)?;
    terminal.set_kitty_image_temp_file_dir(None)?;
    terminal.set_scrollback_max_bytes(Some(SCROLLBACK_BYTES))?;
    // A cursor its program has asked nothing of blinks, as Ghostty's does. The
    // snapshot says whether one was asked for, and one that was is left as it is.
    terminal.set_default_cursor_blink(Some(true))?;
    Ok(())
}

fn run_worker(receiver: flume::Receiver<Command>, shared: &Arc<Shared>, nonce: u64) -> Result<()> {
    let mut input = Input { receiver, bytes: Vec::new(), offset: 0 };
    // The terminal calls back in the middle of a write, so the renderer is shared
    // with that callback and never borrowed across a write or a resize.
    let renderer = Rc::new(RefCell::new(Renderer::new()?));
    // The snapshot streams through bounded chunks on this worker, and the first
    // frame is drawn of its screen, whether or not the pane is on show: a pane
    // has a screen from then on.
    let seen = |terminal: &Terminal<'static, 'static>| {
        renderer.borrow_mut().publish(terminal, shared).map(drop)
    };
    let (mut terminal, more) = match restore(&mut input, seen) {
        Ok(restored) => restored,
        Err(_) if input.receiver.is_disconnected() => return Ok(()),
        Err(error) => return Err(error.context("restoring terminal snapshot")),
    };
    // What came after the screen may show on it: the pixels of a picture, of
    // which Ghostty marks nothing, and the rest of a word cut off at its top.
    if more {
        renderer.borrow_mut().pictured = None;
    }
    // A frame is owed whatever a program holds back: that one, and what the
    // user has scrolled to, selected or found.
    let mut owed = more;
    // A snapshot can be taken inside a hold, of whose beginning nothing will tell.
    renderer.borrow_mut().held = terminal.mode(Mode::SYNC_OUTPUT)?.then(Instant::now);
    terminal.on_render_hold({
        let (renderer, shared) = (renderer.clone(), shared.clone());
        move |terminal, held| {
            let Ok(mut renderer) = renderer.try_borrow_mut() else {
                return;
            };
            renderer.held = held.then(Instant::now);
            // As a hold begins the terminal is exactly the frame its program wants
            // left on screen; what follows in the same write is the next one, half
            // drawn. A program can begin holds without number in one write, so
            // no more of them are drawn than of any other frame, and one that
            // comes too soon after another waits for its hold to end. A failure
            // here shows again when the worker next draws.
            if held
                && shared.shown.load(Ordering::Relaxed)
                && renderer.caught.elapsed() >= FRAME_INTERVAL
                && renderer.publish(terminal, &shared).unwrap_or(false)
            {
                renderer.caught = renderer.drawn;
            }
        }
    })?;
    let mut parser = ApcParser::new(nonce);
    let (mut selecting, mut finding) = (Selecting::new()?, Finding::default());
    let mut caret = Caret::default();
    // The terminal has been written to since text was last looked for in it.
    let mut wrote = false;
    let mut packing = None;
    let mut shown = shared.shown.load(Ordering::Relaxed);
    loop {
        // A pane that comes on show is owed what its terminal has come to.
        let hidden = !shared.shown.load(Ordering::Relaxed);
        owed |= !hidden && !std::mem::replace(&mut shown, !hidden);
        owed |= request::heed(shared, &mut terminal, &mut selecting, &mut finding, &mut caret)?;
        owed |= selecting.tick(&terminal)?;
        let looking = finding.work(&mut terminal, std::mem::take(&mut wrote))?;
        let now = Instant::now();
        let wait = {
            let mut renderer = renderer.borrow_mut();
            let found = finding.found()?;
            // A match found is selected, and the caret takes it up.
            if std::mem::replace(&mut renderer.found, found) != found {
                owed = true;
                caret.adopt(&mut terminal, None)?;
            }
            // A program may have switched screens, whose places the caret's are not.
            caret.follow(&mut terminal)?;
            let viewed = caret.viewed()?;
            owed |= std::mem::replace(&mut renderer.caret, viewed) != viewed;
            // The terminal has no clock, so a hold that never ends is ended here.
            if renderer.held.is_some_and(|since| now.duration_since(since) >= RENDER_HOLD_LIMIT) {
                terminal.set_mode(Mode::SYNC_OUTPUT, false)?;
                renderer.held = None;
            }
            let due = renderer.drawn + FRAME_INTERVAL;
            // Sleep out a hold, or until the next frame is due; once that is
            // published, until more input.
            match renderer.held {
                Some(since) if !owed => Some(since + RENDER_HOLD_LIMIT - now),
                _ if hidden => None,
                _ if now < due => Some(due - now),
                _ => {
                    renderer.publish(&terminal, shared)?;
                    owed = false;
                    None
                }
            }
        };
        // Looking goes on between one input and the next, and so does following
        // a drag held past the top or the bottom, a row a frame.
        let wait = match wait {
            _ if looking => Some(Duration::ZERO),
            _ if selecting.scrolling => Some(wait.unwrap_or(FRAME_INTERVAL).min(FRAME_INTERVAL)),
            None => pack(&mut terminal, &mut packing, now)?,
            _ => wait,
        };
        let Some(command) = input.next(wait) else {
            return Ok(());
        };
        let Command::Bytes(bytes) = command else {
            continue;
        };
        wrote = true;
        for part in parser.feed(&bytes)? {
            match part {
                Part::Output(bytes) => {
                    // The server owns side effects and answers device queries: what
                    // the terminal would say or do of itself here goes unheard.
                    terminal.vt_write(&bytes);
                }
                Part::Metadata(value) => apply_metadata(&mut terminal, &value)?,
            }
        }
    }
}

/// What Ghostty last called a terminal's history, which it calls something else
/// whenever that is written or scrolled through, and when it is then to be
/// packed: at no time, once it has been.
type Packing = Option<(CompressionActivity, Option<Instant>)>;

/// Pack a terminal's history once it has been left alone a while, with nothing
/// else to do: only what a snapshot brings comes packed. A step at a time, each
/// of them short, and this says how long to wait for input before the next.
/// None when there is no more to pack, until its history is touched again.
fn pack(terminal: &mut Terminal, packing: &mut Packing, now: Instant) -> Result<Option<Duration>> {
    let history = terminal.compression_activity()?;
    let mut due = match *packing {
        Some((packed, due)) if packed == history => due,
        _ => Some(now + PACK_AFTER),
    };
    if due.is_some_and(|due| due <= now)
        && terminal.compress(CompressionMode::Incremental)? != CompressionResult::Pending
    {
        due = None;
    }
    *packing = Some((history, due));
    Ok(due.map(|due| due.saturating_duration_since(now)))
}

fn apply_metadata(terminal: &mut Terminal, value: &Value) -> Result<()> {
    match value["type"].as_str() {
        Some("resize") => {
            let dimension = |key: &str| -> Result<u16> {
                value[key]
                    .as_u64()
                    .filter(|v| (1..=65535).contains(v))
                    .map(|v| v as u16)
                    .with_context(|| format!("invalid terminal {key}"))
            };
            let pixels = |key: &str| value[key].as_u64().unwrap_or(0).min(u32::MAX.into()) as u32;
            let cols = dimension("cols")?;
            let rows = dimension("rows")?;
            ensure!(u32::from(cols) * u32::from(rows) <= 4_000_000, "terminal grid is too large");
            terminal.resize(cols, rows, pixels("cell_width_px"), pixels("cell_height_px"))?;
        }
        Some("theme") => {
            // One left out, empty or unreadable is the built-in one.
            let color = |key: &str| value[key].as_str().and_then(|v| Rgb::parse(v).ok());
            terminal.set_default_fg_color(color("foreground"))?;
            terminal.set_default_bg_color(color("background"))?;
            terminal.set_default_cursor_color(color("cursor"))?;
            terminal.set_default_color_palette(None)?;
            if let Some(entries) = value["palette"].as_array() {
                let mut palette = terminal.default_color_palette()?;
                for entry in entries.iter().filter_map(Value::as_str) {
                    if let Ok((index, color)) = Palette::parse_palette_entry(entry) {
                        palette.set(index, color);
                    }
                }
                terminal.set_default_color_palette(Some(palette))?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// The pictures a frame shows, in the order they are drawn: those under the
/// text first, and of those on one side of it the lower first. One that `old`
/// shows keeps the pixels it has there.
fn pictures(
    images: &mut ImageIterator<'static>,
    snapshot: &Snapshot<'static, '_>,
    terminal: &Terminal<'static, '_>,
    old: &[Placed],
) -> Result<Arc<[Placed]>> {
    let storage = terminal.kitty_graphics()?;
    // Ghostty places a picture in pixels, of cells the size it was last told.
    let cell = [
        terminal.width_px()? as f32 / f32::from(terminal.cols()?.max(1)),
        terminal.height_px()? as f32 / f32::from(terminal.rows()?.max(1)),
    ];
    let mut placed: Vec<((bool, i32), Placed)> = Vec::new();
    let mut iteration = images.update(snapshot)?;
    while let Some(image) = iteration.next() {
        let Ok(ImageProtocol::Kitty) = image.protocol() else {
            continue;
        };
        let id = image.image_generation()?;
        let known = old.iter().chain(placed.iter().map(|(_, placed)| placed));
        let known = known.map(|placed| &placed.picture).find(|p| p.id == id);
        let picture = match known {
            Some(picture) => picture.clone(),
            None => {
                let stored = storage.as_ref().and_then(|s| s.image(image.image_id().ok()?));
                let Some(picture) = stored.and_then(|stored| read_picture(&stored, id)) else {
                    continue;
                };
                Arc::new(picture)
            }
        };
        let [wide, tall] = picture.size.map(|side| side as f32);
        let (left, top) = (image.source_x()? as f32, image.source_y()? as f32);
        let part = [
            left / wide,
            top / tall,
            (left + image.source_width()? as f32) / wide,
            (top + image.source_height()? as f32) / tall,
        ];
        let at = [
            image.viewport_col()? as f32 + image.cell_offset_x()? as f32 / cell[0],
            image.viewport_row()? as f32 + image.cell_offset_y()? as f32 / cell[1],
            image.pixel_width()? as f32 / cell[0],
            image.pixel_height()? as f32 / cell[1],
        ];
        let under = image.layer()? != ImageLayer::AboveText;
        let order = (!under, image.z()?);
        let picture = Placed { picture, under, at, part };
        placed.push((order, picture));
    }
    placed.sort_by_key(|(order, _)| *order);
    Ok(placed.into_iter().map(|(_, placed)| placed).collect())
}

/// A stored picture's pixels as four bytes each, if they have all arrived.
fn read_picture(stored: &graphics::Image, id: u64) -> Option<Picture> {
    let size = [stored.width().ok()? as usize, stored.height().ok()? as usize];
    let data = stored.data().ok()??;
    let each = match stored.format().ok()? {
        ImageFormat::Rgba => 4,
        ImageFormat::Rgb => 3,
        ImageFormat::GrayAlpha => 2,
        ImageFormat::Gray => 1,
        // Ghostty stores what a PNG decodes to.
        _ => return None,
    };
    if size[0] * size[1] == 0 || data.len() != size[0] * size[1] * each {
        return None;
    }
    let pixel = |from: &[u8]| match *from {
        [r, g, b, a] => [r, g, b, a],
        [r, g, b] => [r, g, b, 255],
        [grey, a] => [grey, grey, grey, a],
        _ => [from[0], from[0], from[0], 255],
    };
    let rgba = data.chunks_exact(each).flat_map(pixel).collect();
    Some(Picture { id, size, rgba })
}

fn rgb(color: Rgb) -> u32 {
    (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
}

/// Read a row as Ghostty holds it. What a cell is asked, it is asked across a call
/// into Ghostty, and those calls are most of what a frame costs: so the row is
/// asked what none of its cells need be, and a cell only what the one before it
/// does not tell.
fn read(
    line: &RowIteration<'static, '_>,
    cells: &mut CellIterator<'static>,
    terminal: &Terminal,
    colors: &Colors,
    row: &mut Row,
) -> Result<()> {
    let about = line.raw_row()?;
    let styled = about.is_styled()?;
    let clustered = about.has_grapheme_cluster()?;
    let linked = about.has_hyperlink()?;
    row.cells.clear();
    row.text.clear();
    row.looks.clear();
    row.links.clear();
    row.wrapped = about.is_wrapped()?;
    let mut asked = cells.update(line)?;
    // What the last look was taken from, whether it hides its text, and a cell
    // that has it.
    let (mut from, mut hidden, mut like) = (None, false, Cell::default());
    // The cell before, where one just like it is drawn just like it.
    let mut before = None;
    let mut cluster = String::new();
    let mut address = [0; MAX_LINK_BYTES];
    for (x, raw) in (0u16..).zip(line.cells_raw()?) {
        let at = row.text.len() as u32;
        // The blank rest of a row is one cell over and over.
        if before == Some(raw)
            && let Some(cell) = row.cells.last().copied()
        {
            if cell.at < at {
                row.text.extend_from_within(cell.at as usize..);
            }
            row.cells.push(Cell { at, ..cell });
            continue;
        }
        let codepoint = raw.codepoint()?;
        let blank = codepoint == 0;
        let style = styled.then(|| raw.style_id()).transpose()?;
        // And most of the others hold one character, in the style of the cell
        // before.
        if !(blank || clustered || linked || hidden)
            && from == Some((style, None))
            && let Some(shown) = char::from_u32(codepoint)
        {
            row.text.push(shown);
            row.cells.push(Cell { at, ..like });
            before = Some(raw);
            continue;
        }
        let tag = if blank || clustered { raw.content_tag()? } else { CellContentTag::Codepoint };
        // Cells of one style look alike, but for a blank one that holds a colour
        // of its own to lie on.
        let ground = (blank && tag != CellContentTag::Codepoint).then_some(raw);
        if from != Some((style, ground)) {
            from = Some((style, ground));
            asked.select(x)?;
            let style = asked.style()?;
            let (mut fg, mut bg) = (
                rgb(asked.fg_color()?.unwrap_or(colors.foreground)),
                rgb(asked.bg_color()?.unwrap_or(colors.background)),
            );
            if style.inverse {
                std::mem::swap(&mut fg, &mut bg);
            }
            let underline_color = match style.underline_color {
                StyleColor::None => None,
                StyleColor::Palette(index) => Some(rgb(colors.palette[usize::from(index.0)])),
                StyleColor::Rgb(color) => Some(rgb(color)),
            };
            hidden = style.invisible;
            like.look = row.looks.len() as u16;
            row.looks.push(Look { fg, bg, underline_color, style });
        }
        if blank || hidden {
        } else if tag == CellContentTag::CodepointGrapheme {
            asked.select(x)?;
            asked.graphemes_utf8(&mut cluster)?;
            row.text.push_str(&cluster);
        } else if let Some(shown) = char::from_u32(codepoint) {
            row.text.push(shown);
        }
        // Only a blank cell is the unused half of a wide glyph.
        let wide = if blank { raw.wide()? } else { CellWide::Narrow };
        let link = linked && raw.has_hyperlink()?;
        if link && let Ok(y) = u32::try_from(line.viewport_y()?) {
            let cell = Point::Viewport(Coordinate { x, y });
            // One too long to hold is no link.
            let length = terminal.grid_ref(cell)?.hyperlink_uri(&mut address);
            let length = length.ok().filter(|length| *length > 0);
            let target = length.map(|length| &address[..length]);
            if let Some(Ok(target)) = target.map(str::from_utf8) {
                let x = usize::from(x);
                match row.links.last_mut() {
                    Some((_, end, last)) if *end == x && **last == *target => *end += 1,
                    _ => row.links.push((x, x + 1, target.into())),
                }
            }
        }
        row.cells.push(Cell {
            at,
            spacer_tail: wide == CellWide::SpacerTail,
            spacer_head: wide == CellWide::SpacerHead,
            ..like
        });
        // What a cluster holds beside its first character, and where a link
        // leads, the cell itself does not say.
        before = (tag != CellContentTag::CodepointGrapheme && !link).then_some(raw);
    }
    Ok(())
}

struct Renderer {
    state: RenderState<'static>,
    rows: RowIterator<'static>,
    cells: CellIterator<'static>,
    images: ImageIterator<'static>,
    /// What Ghostty called the list of pictures when it was last read.
    pictured: Option<u64>,
    previous: Option<Arc<Frame>>,
    /// The rows of that frame, by what Ghostty knows each as wherever it moves to.
    known: HashMap<RowId, Arc<Row>>,
    /// Where a row is read into, so that one found unchanged asks for no memory.
    read: Row,
    /// The colours of that frame. Ghostty marks nothing dirty when only they change.
    colors: Option<Colors>,
    /// What the next frame is to say of the text looked for.
    found: Option<(usize, usize)>,
    /// And of the caret: whether there is one, and where it is in view if it is.
    caret: Option<Option<(u16, u16)>>,
    /// When the last new frame was drawn, when the last was drawn as a hold began,
    /// and since when its program has held frames back (synchronized output, DEC
    /// mode 2026).
    drawn: Instant,
    caught: Instant,
    held: Option<Instant>,
}

impl Renderer {
    fn new() -> Result<Self> {
        Ok(Self {
            state: RenderState::new()?,
            rows: RowIterator::new()?,
            cells: CellIterator::new()?,
            images: ImageIterator::new()?,
            pictured: None,
            previous: None,
            known: HashMap::new(),
            read: Row::default(),
            colors: None,
            found: None,
            caret: None,
            drawn: Instant::now() - FRAME_INTERVAL,
            caught: Instant::now() - FRAME_INTERVAL,
            held: None,
        })
    }

    /// Draw the terminal as it stands, and hand the UI the frame if it is a new one.
    fn publish(&mut self, terminal: &Terminal<'static, '_>, shared: &Shared) -> Result<bool> {
        let Some(frame) = self.frame(terminal)? else {
            return Ok(false);
        };
        *shared.frame.write().unwrap() = Some(frame);
        (shared.repaint)();
        self.drawn = Instant::now();
        Ok(true)
    }

    fn frame(&mut self, terminal: &Terminal<'static, '_>) -> Result<Option<Arc<Frame>>> {
        let snapshot = self.state.update(terminal)?;
        let scrollbar = terminal.scrollbar()?;
        let scroll_offset =
            scrollbar.total.saturating_sub(scrollbar.offset + scrollbar.len) as usize;
        let mouse_tracking = terminal.is_mouse_tracking()?;
        let alternate_screen = terminal.active_screen()? == Screen::Alternate;
        let alternate_scroll = terminal.mode(Mode::ALT_SCROLL)?;
        let dirty = snapshot.dirty()?;
        let cursor = snapshot.cursor()?;
        let cursor_shape = match cursor.visual_style {
            CursorVisualStyle::Bar => CursorShape::Bar,
            CursorVisualStyle::Underline => CursorShape::Underline,
            CursorVisualStyle::BlockHollow => CursorShape::Hollow,
            _ => CursorShape::Block,
        };
        let cursor_blink = cursor.blinking;
        let cursor = cursor.viewport.filter(|_| cursor.visible).map(|p| (p.x, p.y));
        // The caret takes the cursor's place while there is one: a block, still.
        let (cursor, cursor_shape, cursor_blink) = match self.caret {
            Some(caret) => (caret, CursorShape::Block, false),
            None => (cursor, cursor_shape, cursor_blink),
        };
        let colors = snapshot.colors()?;
        let recolored = self.colors.as_ref() != Some(&colors);
        // Nor when only a picture comes, goes or moves.
        let pictured = Some(snapshot.image_placements_generation()?);
        let repictured = self.pictured != pictured;
        // Ghostty marks nothing dirty when only the cursor, the colours or a mode
        // changes. It marks all of it dirty when the view moves, by the user's hand
        // or because its history was cut away, and the rows a clear shifts. History
        // that only grows under a view scrolled back changes how far back that is
        // and nothing in it, so that alone is no new frame.
        if dirty == Dirty::Clean
            && !recolored
            && !repictured
            && self.previous.as_ref().is_some_and(|old| {
                (old.cursor, old.cursor_shape, old.cursor_blink)
                    == (cursor, cursor_shape, cursor_blink)
                    && (old.scroll_offset == 0) == (scroll_offset == 0)
                    && old.mouse_tracking == mouse_tracking
                    && old.alternate_screen == alternate_screen
                    && old.alternate_scroll == alternate_scroll
                    && old.found == self.found
                    && old.caret == self.caret.is_some()
            })
        {
            return Ok(None);
        }
        let fg = rgb(colors.foreground);
        let bg = rgb(colors.background);
        let cols = snapshot.cols()?;
        let rows = snapshot.rows()?;
        // Every look holds the colours it is drawn in, so new ones are new rows.
        // A cursor that only moved leaves every row as it was.
        let full = dirty == Dirty::Full || recolored;
        let mut lines = Vec::with_capacity(usize::from(rows));
        let mut known = HashMap::with_capacity(usize::from(rows));
        // A row is asked what is selected of it only when something is, and its
        // text is none the newer for that.
        let selecting = terminal.selection()?.is_some();
        let mut selected = Vec::new();
        let mut iterator = self.rows.update(&snapshot)?;
        while let Some(line) = iterator.next() {
            let id = line.id()?;
            let row = match self.known.remove(&id) {
                Some(old) if !full && !line.dirty()? => old,
                old => {
                    // Ghostty calls every row dirty when the view moves, and so
                    // forgets which were written to. One that only moved reads
                    // as it did.
                    let row = &mut self.read;
                    read(line, &mut self.cells, terminal, &colors, row)?;
                    old.filter(|old| **old == *row).unwrap_or_else(|| Arc::new(row.clone()))
                }
            };
            if selecting {
                // Ghostty ends at the first half of a wide glyph, which is selected whole.
                let tail = |x: u16| row.cells.get(usize::from(x)).is_some_and(|c| c.spacer_tail);
                let part = line.selection()?.map(|part| (part.start_x, part.end_x + 1));
                selected.push(part.map(|(start, end)| (start, end + u16::from(tail(end)))));
            }
            known.insert(id, row.clone());
            lines.push(row);
        }
        self.known = known;
        let beyond = beyond(terminal, &lines, cols, scrollbar.offset, scrollbar.total)?;
        let old = self.previous.as_ref().map(|old| &old.pictures);
        let pictures = match old {
            Some(old) if !repictured => old.clone(),
            old => {
                let old = old.map_or(&[][..], |old| old);
                pictures(&mut self.images, &snapshot, terminal, old)?
            }
        };
        self.pictured = pictured;
        let frame = Arc::new(Frame {
            cols,
            rows,
            lines,
            fg,
            bg,
            scroll_offset,
            history: scrollbar.total.saturating_sub(scrollbar.len) as usize,
            mouse_tracking,
            alternate_screen,
            alternate_scroll,
            cursor,
            cursor_color: colors.cursor.map(rgb).unwrap_or(fg),
            ansi: [1, 2, 3, 4].map(|index: usize| rgb(colors.palette[index])),
            cursor_shape,
            cursor_blink,
            selected,
            found: self.found,
            caret: self.caret.is_some(),
            pictures,
            beyond,
        });
        snapshot.clean()?;
        self.previous = Some(frame.clone());
        self.colors = Some(colors);
        Ok(Some(frame))
    }
}

/// What of a word is out of sight above the rows shown, and below them, as
/// `Frame::beyond` has it. `top` is the first of those rows among all the
/// `total` rows the screen holds, its history with it.
fn beyond(
    terminal: &Terminal<'static, '_>,
    lines: &[Arc<Row>],
    cols: u16,
    top: u64,
    total: u64,
) -> Result<[Option<String>; 2]> {
    let at = |x: u16, y: u64| {
        let y = u32::try_from(y)?;
        anyhow::Ok(terminal.grid_ref(Point::Screen(Coordinate { x, y }))?)
    };
    let wraps = |y: u64| anyhow::Ok(at(0, y)?.row()?.is_wrapped()?);
    // What a cell shows, as so many characters of `cluster`: none for the idle
    // half of a wide character, and None for a blank, where a word ends.
    let mut cluster = ['\0'; 8];
    let shown = |x: u16, y: u64, cluster: &mut [char; 8]| {
        let cell = at(x, y)?;
        let length = cell.graphemes(cluster).unwrap_or(0);
        // Only a cell with nothing in it is asked whether it is such a half.
        if length == 0 {
            let wide = cell.cell()?.wide()?;
            let half = matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead);
            return anyhow::Ok(half.then_some(0));
        }
        let blank = cluster[..length].iter().all(|c| c.is_whitespace());
        Ok((!blank).then_some(length))
    };
    let blank = |row: &Row, x: usize| {
        let half = |cell: &Cell| cell.spacer_tail || cell.spacer_head;
        row.text(x, x + 1).trim().is_empty() && !row.cells.get(x).is_some_and(half)
    };
    let held = |text: Vec<char>| (text.len() <= MAX_LINK_BYTES).then(|| String::from_iter(text));
    // Back from the first row's first cell, over the rows that wrap into it.
    let mut above = Some(String::new());
    if let Some(first) = lines.first()
        && !blank(first, 0)
    {
        let (mut text, mut x, mut y) = (Vec::new(), 0, top);
        while text.len() <= MAX_LINK_BYTES {
            if x == 0 {
                if y == 0 || !wraps(y - 1)? {
                    break;
                }
                (x, y) = (cols, y - 1);
            }
            x -= 1;
            let Some(length) = shown(x, y, &mut cluster)? else {
                break;
            };
            text.extend(cluster[..length].iter().rev());
        }
        text.reverse();
        above = held(text);
    }
    // On from the last row's last cell, over the rows it wraps into.
    let mut below = Some(String::new());
    if let Some(last) = lines.last()
        && last.wrapped
        && !blank(last, last.cells.len().saturating_sub(1))
    {
        let (mut text, mut x, mut y) = (Vec::new(), 0, top + lines.len() as u64);
        while text.len() <= MAX_LINK_BYTES && y < total {
            if x == cols {
                if !wraps(y)? {
                    break;
                }
                (x, y) = (0, y + 1);
                continue;
            }
            let Some(length) = shown(x, y, &mut cluster)? else {
                break;
            };
            text.extend(&cluster[..length]);
            x += 1;
        }
        below = held(text);
    }
    Ok([above, below])
}

#[cfg(test)]
#[path = "../tests/unit/terminal.rs"]
pub(crate) mod tests;
