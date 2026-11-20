//! The look: a neutral dark palette, a few flat controls, Inter and JetBrains Mono.
use std::sync::Arc;

use crate::program::Program;
use eframe::egui::{
    self, Align2, Color32, FontData, FontFamily, FontId, Pos2, Rect, Sense, Stroke, StrokeKind,
    Vec2,
    emath::GuiRounding,
    epaint::FontColorTransferFunction,
    epaint::text::{FontTweak, VariationCoords},
    text::{LayoutJob, TextWrapping},
};

/// The window's ground where no terminal lends its own, and a card that has
/// no terminal to take its ground from.
const VIEW: Color32 = Color32::from_rgb(0x16, 0x16, 0x19);
pub const CARD: Color32 = Color32::from_rgb(0x0b, 0x0b, 0x0d);
const POPOVER: Color32 = Color32::from_rgb(0x26, 0x26, 0x2b);
pub const TEXT: Color32 = Color32::WHITE;
pub const DIM: Color32 = white(150);
pub const BORDER: Color32 = white(16);
/// The line around the window, which has to part it from whatever lies behind.
pub const EDGE_LINE: Color32 = white(34);
/// What marks the one in use: light enough to be a line or a dot on the ground.
const ACCENT: Color32 = Color32::from_rgb(0x5b, 0x9c, 0xf5);
/// Darker, to carry white text.
const ACCENT_FILL: Color32 = Color32::from_rgb(0x35, 0x84, 0xe4);
/// Something is waiting for the user.
const AMBER: Color32 = Color32::from_rgb(0xf5, 0xb0, 0x2e);
/// Something has failed.
const RED: Color32 = Color32::from_rgb(0xf6, 0x61, 0x51);
/// Something has finished.
const GREEN: Color32 = Color32::from_rgb(0x57, 0xe3, 0x89);
/// What is said under a name: fainter than `DIM`.
const FAINT: Color32 = white(124);
/// Fills of flat controls: selected, and pressed. Under the pointer one is as
/// white as `HOVER` says, of 255.
const HOVER: f32 = 18.0;
pub const ACTIVE: Color32 = white(28);
pub const PRESSED: Color32 = white(46);
/// Fills of rows, which lie many together: faint under the pointer, so that the
/// one selected is plain among them.
const OVER: f32 = 12.0;
const SELECTED: f32 = 26.0;
/// How long a fill takes to come and to go, and a menu to appear, in seconds.
pub const EASE: f32 = 0.1;
/// The fill of what ends programs if pressed. Darker than `RED`, to carry white text.
pub const DANGER: Color32 = Color32::from_rgb(0xc0, 0x1c, 0x28);

/// The colours that mean something in the window, which go by the theme of the
/// terminal on show where it has ones that can be told from its ground.
#[derive(Clone, Copy, PartialEq)]
pub struct Tints {
    /// What marks the one in use, and work under way.
    pub accent: Color32,
    pub done: Color32,
    pub waiting: Color32,
    pub failed: Color32,
}

impl Default for Tints {
    fn default() -> Self {
        Self { accent: ACCENT, done: GREEN, waiting: AMBER, failed: RED }
    }
}

impl Tints {
    /// Those of a theme: its blue, green, yellow and red, each where it stands
    /// out from the theme's `ground` as a small dot or a thin line must.
    pub fn themed(ground: Color32, [red, green, yellow, blue]: [Color32; 4]) -> Self {
        let clear = |theirs: Color32, ours: Color32| match luma(theirs) - luma(ground) >= 80 {
            true => theirs,
            false => ours,
        };
        let ours = Self::default();
        Self {
            accent: clear(blue, ours.accent),
            done: clear(green, ours.done),
            waiting: clear(yellow, ours.waiting),
            failed: clear(red, ours.failed),
        }
    }
}

/// The colours in force.
pub fn tints(ctx: &egui::Context) -> Tints {
    ctx.data(|data| data.get_temp(egui::Id::NULL)).unwrap_or_default()
}

/// Put the colours that go by the terminal on show in force, which is known by
/// its background and its red, green, yellow and blue: what is laid over the
/// window, and what means something in it. The window's ground, which goes by
/// the terminal too, is given back.
pub fn theme(ctx: &egui::Context, terminal: Option<(Color32, [Color32; 4])>) -> Color32 {
    let (ground, popover, tints) = match terminal {
        Some((back, ansi)) => (ground(back), popover(back), Tints::themed(back, ansi)),
        None => (VIEW, POPOVER, Tints::default()),
    };
    if ctx.global_style().visuals.window_fill != popover || self::tints(ctx) != tints {
        ctx.data_mut(|data| data.insert_temp(egui::Id::NULL, tints));
        ctx.all_styles_mut(|style| {
            style.visuals.window_fill = popover;
            select(&mut style.visuals, tints.accent);
        });
    }
    ground
}

/// What is selected in a field lies on the accent, and is written in a tint of
/// it light enough to read there. egui outlines the field in use in the same.
fn select(visuals: &mut egui::Visuals, accent: Color32) {
    visuals.selection.bg_fill = accent.gamma_multiply(0.5);
    visuals.selection.stroke = Stroke::new(1.0, accent.lerp_to_gamma(Color32::WHITE, 0.75));
}

/// Translucent white, for text and fills that sit on any ground.
const fn white(alpha: u8) -> Color32 {
    Color32::from_rgba_premultiplied(alpha, alpha, alpha, alpha)
}

/// How far a fill has come, from none to all of it, that is there while `on`:
/// it takes a moment over it either way. `id` is whose fill it is.
pub fn eased(ui: &egui::Ui, id: egui::Id, on: bool) -> f32 {
    ui.ctx().animate_bool_with_time(id, on, EASE)
}

pub const TOOLBAR: f32 = 40.0;
/// How tall a row of the sidebar is, which says a line under its name.
pub const ROW: f32 = 44.0;
/// How far the middle of each of a row's two lines is from the row's own.
const LINE: f32 = 9.0;
pub const BODY: f32 = 13.5;
pub const SMALL: f32 = 12.0;
const RADIUS: u8 = 7;
/// A card's corners, and the space between two cards and around them all.
pub const CARD_RADIUS: u8 = 9;
pub const GUTTER: f32 = 6.0;
/// The space to each side of the word in a button that asks.
const PILL: f32 = 8.0;
pub const SMALL_BUTTON: f32 = 24.0;
/// The side of a program's icon: a tile, wider than it is tall, on a square.
pub const TILE: f32 = 20.0;
/// And on a row of two lines.
const LARGE: f32 = 28.0;
/// The room an icon takes ahead of the name it goes with.
pub const ICON: f32 = TILE + 7.0;

pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}

/// What surrounds a dark terminal is its background, a little lighter, so that
/// the terminal lies on it as a card of the same hue. The light ink that is
/// written there would be lost around a light terminal, which gets the plain ground.
fn ground(terminal: Color32) -> Color32 {
    lifted(terminal, 55).unwrap_or(VIEW)
}

/// What is laid over the window, a menu or a note, is of the same hue again and
/// lighter than the ground, to stand off it.
fn popover(terminal: Color32) -> Color32 {
    lifted(terminal, 115).unwrap_or(POPOVER)
}

/// How light a colour is to the eye, of 255.
fn luma(color: Color32) -> i32 {
    let [r, g, b, _] = color.to_array().map(i32::from);
    (r * 299 + g * 587 + b * 114) / 1000
}

/// A dark colour brought so many thousandths of the way to white, and no light one.
fn lifted(color: Color32, by: u32) -> Option<Color32> {
    let [r, g, b, _] = color.to_array().map(u32::from);
    let lift = |channel: u32| (channel + (255 - channel) * by / 1000) as u8;
    (luma(color) < 64).then(|| Color32::from_rgb(lift(r), lift(g), lift(b)))
}

pub fn apply(ctx: &egui::Context) {
    // Each variable font serves as a regular and a bold face, and egui's own
    // fonts stay behind them for symbols. What the slanted faces cannot draw,
    // the upright one does before any other: it is set up before them for that.
    let mut fonts = egui::FontDefinitions::default();
    let inter = FontData::from_static(include_bytes!("../../assets/fonts/Inter.ttf"));
    let mono = FontData::from_static(include_bytes!("../../assets/fonts/JetBrainsMono.ttf"));
    let slanted =
        FontData::from_static(include_bytes!("../../assets/fonts/JetBrainsMono-Italic.ttf"));
    let (words, code, own) =
        (FontFamily::Proportional, FontFamily::Monospace, super::fonts::family);
    let faces = [
        (words.clone(), [words, FontFamily::Name("bold".into())], inter),
        (code.clone(), [own(false, false), own(true, false)], mono),
        (code, [own(false, true), own(true, true)], slanted),
    ];
    for (behind, families, face) in faces {
        let fallbacks = fonts.families[&behind].clone();
        for (family, weight) in families.into_iter().zip([400.0, 700.0]) {
            let name = format!("{family} {weight}");
            let tweak = FontTweak {
                coords: VariationCoords::new([(b"wght", weight)]),
                ..Default::default()
            };
            let data = face.clone().tweak(tweak);
            fonts.font_data.insert(name.clone(), Arc::new(data));
            let stack = [name].into_iter().chain(fallbacks.clone()).collect();
            fonts.families.insert(family, stack);
        }
    }
    ctx.set_fonts(fonts);

    ctx.all_styles_mut(|style| {
        style.visuals = egui::Visuals::dark();
        let visuals = &mut style.visuals;
        visuals.panel_fill = Color32::TRANSPARENT;
        // Light text on a dark ground is drawn heavier than its outlines by
        // default, which makes a regular weight read as a bold one. This keeps
        // it nearer what the font's designer drew.
        visuals.text_options.color_transfer_function = FontColorTransferFunction::Gamma(0.72);
        visuals.window_fill = POPOVER;
        visuals.window_stroke = Stroke::new(1.0, white(20));
        visuals.window_corner_radius = 12.into();
        visuals.menu_corner_radius = 10.into();
        visuals.popup_shadow = egui::Shadow {
            offset: [0, 4],
            blur: 18,
            spread: 0,
            color: Color32::from_black_alpha(96),
        };
        visuals.extreme_bg_color = ACTIVE;
        select(visuals, ACCENT);
        visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        visuals.widgets.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
        let states = [
            (&mut visuals.widgets.inactive, ACTIVE),
            (&mut visuals.widgets.hovered, white(38)),
            (&mut visuals.widgets.active, PRESSED),
            (&mut visuals.widgets.open, PRESSED),
        ];
        for (widget, fill) in states {
            widget.weak_bg_fill = fill;
            widget.bg_fill = fill;
            widget.bg_stroke = Stroke::NONE;
            widget.fg_stroke = Stroke::new(1.0, TEXT);
            widget.corner_radius = RADIUS.into();
            widget.expansion = 0.0;
        }
        style.spacing.item_spacing = egui::vec2(6.0, 6.0);
        style.spacing.button_padding = egui::vec2(12.0, 7.0);
        style.spacing.interact_size.y = 34.0;
        style.text_styles = [
            (egui::TextStyle::Body, FontId::proportional(BODY)),
            (egui::TextStyle::Button, FontId::proportional(BODY)),
            (egui::TextStyle::Small, FontId::proportional(SMALL)),
            (egui::TextStyle::Monospace, FontId::monospace(13.5)),
            (egui::TextStyle::Heading, bold(24.0)),
        ]
        .into();
        style.animation_time = EASE;
    });
    // A terminal owns Ctrl+/- itself. Avoid a second, implicit zoom of the whole UI.
    ctx.options_mut(|o| o.zoom_with_keyboard = false);
}

#[derive(Clone, Copy)]
pub enum Icon {
    Sidebar,
    Plus,
    SplitRight,
    SplitDown,
    Retry,
    Close,
    /// Let a pane fill its tab, and give the tab back to them all.
    Zoom,
    Restore,
    /// Panes that are out of sight.
    Behind,
    Down,
    /// A shell's, for a program whose icon is not at hand.
    Prompt,
    Dot,
    /// A dot that has been silenced.
    Muted,
    /// The window's own: out of the way, and over the whole screen or back.
    Minimize,
    Maximize,
    /// A glass to find things with.
    Search,
    /// A list of things, each with its mark: what is at work.
    Agents,
    /// An eye, to look at something from where one is.
    Peek,
    /// A pane over the corner of another: one that floats.
    Float,
    /// The pictures an action gives the things it offers, with `Plus`, and
    /// one for a thing whose picture is not among them.
    Document,
    Folder,
    Branch,
    Terminal,
    Code,
    Server,
    Database,
    Package,
    Globe,
    Link,
    Person,
    Tag,
    Clock,
    Star,
    Gear,
    Thing,
}

impl Icon {
    /// The picture an action names for a thing it offers: none for no name.
    pub fn named(name: &str) -> Option<Self> {
        Some(match name {
            "" => return None,
            "document" => Self::Document,
            "folder" => Self::Folder,
            "branch" => Self::Branch,
            "terminal" => Self::Terminal,
            "code" => Self::Code,
            "server" => Self::Server,
            "database" => Self::Database,
            "package" => Self::Package,
            "globe" => Self::Globe,
            "link" => Self::Link,
            "person" => Self::Person,
            "tag" => Self::Tag,
            "clock" => Self::Clock,
            "star" => Self::Star,
            "gear" => Self::Gear,
            "plus" => Self::Plus,
            _ => Self::Thing,
        })
    }

    fn paint(self, painter: &egui::Painter, center: Pos2, color: Color32) {
        let stroke = Stroke::new(1.5, color);
        let line = |from: [f32; 2], to: [f32; 2]| {
            painter.line_segment([center + Vec2::from(from), center + Vec2::from(to)], stroke);
        };
        // A line through several points, a circle and one filled in, a box
        // with round corners, and part of an oval: so many twenty-fourths of
        // the way round it, clockwise from its right.
        let path = |points: &[[f32; 2]]| {
            painter.line(points.iter().map(|at| center + Vec2::from(*at)).collect(), stroke);
        };
        let ring = |at: [f32; 2], radius: f32| {
            painter.circle_stroke(center + Vec2::from(at), radius, stroke);
        };
        let dot = |at: [f32; 2], radius: f32| {
            painter.circle_filled(center + Vec2::from(at), radius, color);
        };
        let frame = |at: [f32; 2], size: [f32; 2], radius: f32| {
            let rect = Rect::from_center_size(center + Vec2::from(at), size.into());
            painter.rect_stroke(rect, radius, stroke, StrokeKind::Inside);
        };
        let bow = |at: [f32; 2], radii: [f32; 2], part: std::ops::RangeInclusive<i32>| {
            let around = part.map(|step| {
                let turned = Vec2::angled(step as f32 * std::f32::consts::TAU / 24.0);
                center + Vec2::from(at) + turned * Vec2::from(radii)
            });
            painter.line(around.collect(), stroke);
        };
        let window = || frame([0.0, 0.0], [16.0, 14.0], 3.0);
        match self {
            Icon::Plus => {
                line([-6.0, 0.0], [6.0, 0.0]);
                line([0.0, -6.0], [0.0, 6.0]);
            }
            Icon::Sidebar => {
                window();
                line([-2.5, -6.0], [-2.5, 6.0]);
            }
            Icon::SplitRight => {
                window();
                line([0.0, -6.0], [0.0, 6.0]);
            }
            Icon::SplitDown => {
                window();
                line([-7.0, 0.0], [7.0, 0.0]);
            }
            Icon::Float => {
                window();
                frame([0.0, 0.0], [8.0, 6.0], 1.5);
            }
            Icon::Close => {
                line([-4.0, -4.0], [4.0, 4.0]);
                line([-4.0, 4.0], [4.0, -4.0]);
            }
            // Two corners, drawn apart or drawn together.
            Icon::Zoom | Icon::Restore => {
                let (corner, arm) = match self {
                    Icon::Zoom => (5.0, -4.0),
                    _ => (2.5, 4.0),
                };
                for side in [1.0, -1.0] {
                    let (x, y) = (side * corner, -side * corner);
                    line([x, y], [x + side * arm, y]);
                    line([x, y], [x, y - side * arm]);
                }
            }
            // One window, and the edges of another that shows from behind it.
            Icon::Behind => {
                frame([-1.5, 1.5], [13.0, 11.0], 3.0);
                path(&[[-4.0, -6.5], [7.5, -6.5], [7.5, 3.0]]);
            }
            Icon::Down => path(&[[-4.0, -2.0], [0.0, 2.0], [4.0, -2.0]]),
            Icon::Prompt => {
                path(&[[-6.0, -4.0], [-2.0, 0.0], [-6.0, 4.0]]);
                line([0.5, 4.5], [6.0, 4.5]);
            }
            Icon::Dot => dot([0.0, 0.0], 3.5),
            Icon::Muted => {
                ring([0.0, 0.0], 3.5);
                line([-5.0, 5.0], [5.0, -5.0]);
            }
            Icon::Agents => {
                for row in [-5.0, 0.0, 5.0] {
                    dot([-5.5, row], 1.5);
                    line([-1.5, row], [7.0, row]);
                }
            }
            Icon::Peek => {
                // Two lids, each bowed from one corner of the eye to the other.
                for bowed in [-4.5, 4.5] {
                    let lid = |at: f32| center + egui::vec2(at, bowed * (1.0 - (at / 7.0).powi(2)));
                    painter.line((-7..=7).map(|at| lid(at as f32)).collect(), stroke);
                }
                dot([0.0, 0.0], 2.0);
            }
            Icon::Search => {
                ring([-1.5, -1.5], 4.5);
                line([2.0, 2.0], [6.0, 6.0]);
            }
            Icon::Document => {
                path(&[
                    [1.5, -7.0],
                    [-5.0, -7.0],
                    [-5.0, 7.0],
                    [5.0, 7.0],
                    [5.0, -3.5],
                    [1.5, -7.0],
                ]);
                path(&[[1.5, -7.0], [1.5, -3.5], [5.0, -3.5]]);
            }
            Icon::Folder => path(&[
                [-7.0, -5.5],
                [-2.5, -5.5],
                [-0.5, -3.5],
                [7.0, -3.5],
                [7.0, 5.5],
                [-7.0, 5.5],
                [-7.0, -5.5],
            ]),
            // A line from one dot to another, and a third that joins it.
            Icon::Branch => {
                for at in [[-4.0, -5.0], [-4.0, 5.0], [4.0, -3.0]] {
                    ring(at, 2.0);
                }
                line([-4.0, -3.0], [-4.0, 3.0]);
                path(&[[4.0, -1.0], [4.0, 0.0], [1.5, 1.5], [-4.0, 2.0]]);
            }
            Icon::Terminal => {
                window();
                path(&[[-4.5, -2.5], [-2.0, 0.0], [-4.5, 2.5]]);
                line([0.0, 2.5], [4.0, 2.5]);
            }
            Icon::Code => {
                path(&[[-3.0, -4.5], [-7.0, 0.0], [-3.0, 4.5]]);
                path(&[[3.0, -4.5], [7.0, 0.0], [3.0, 4.5]]);
                line([1.2, -6.0], [-1.2, 6.0]);
            }
            // Two machines in a rack, each with its light.
            Icon::Server => {
                for row in [-3.75, 3.75] {
                    frame([0.0, row], [14.0, 6.0], 2.0);
                    dot([-4.0, row], 1.0);
                }
            }
            // A drum, with a band round its middle.
            Icon::Database => {
                bow([0.0, -4.0], [6.0, 2.5], 0..=24);
                bow([0.0, 0.0], [6.0, 2.5], 0..=12);
                bow([0.0, 4.0], [6.0, 2.5], 0..=12);
                line([-6.0, -4.0], [-6.0, 4.0]);
                line([6.0, -4.0], [6.0, 4.0]);
            }
            // A box, seen from a corner.
            Icon::Package => {
                path(&[
                    [0.0, -7.0],
                    [6.0, -3.5],
                    [6.0, 3.5],
                    [0.0, 7.0],
                    [-6.0, 3.5],
                    [-6.0, -3.5],
                    [0.0, -7.0],
                ]);
                path(&[[-6.0, -3.5], [0.0, 0.0], [6.0, -3.5]]);
                line([0.0, 0.0], [0.0, 7.0]);
            }
            Icon::Globe => {
                ring([0.0, 0.0], 6.5);
                bow([0.0, 0.0], [3.0, 6.5], 0..=24);
                line([-6.5, 0.0], [6.5, 0.0]);
            }
            // Two links of a chain.
            Icon::Link => {
                frame([-3.0, 0.0], [8.0, 6.0], 3.0);
                frame([3.0, 0.0], [8.0, 6.0], 3.0);
            }
            Icon::Person => {
                ring([0.0, -3.5], 3.0);
                bow([0.0, 7.0], [6.0, 5.5], 12..=24);
            }
            // A label with a hole for its string.
            Icon::Tag => {
                path(&[
                    [-6.5, -6.5],
                    [0.0, -6.5],
                    [6.5, 0.0],
                    [0.0, 6.5],
                    [-6.5, 0.0],
                    [-6.5, -6.5],
                ]);
                dot([-3.2, -3.2], 1.1);
            }
            Icon::Clock => {
                ring([0.0, 0.0], 6.5);
                path(&[[0.0, -4.0], [0.0, 0.0], [3.0, 1.5]]);
            }
            // Five points, and the five hollows between them.
            Icon::Star => {
                let around = (0..=10).map(|step| {
                    let turned = Vec2::angled((step as f32 / 10.0 - 0.25) * std::f32::consts::TAU);
                    center + turned * if step % 2 == 0 { 7.0 } else { 3.0 }
                });
                painter.line(around.collect(), stroke);
            }
            Icon::Gear => {
                ring([0.0, 0.0], 4.5);
                ring([0.0, 0.0], 1.5);
                for tooth in 0..8 {
                    let turned = Vec2::angled(tooth as f32 * std::f32::consts::TAU / 8.0);
                    line((turned * 4.5).into(), (turned * 7.0).into());
                }
            }
            Icon::Thing => frame([0.0, 0.0], [11.0, 11.0], 3.0),
            Icon::Minimize => line([-4.0, 3.5], [4.0, 3.5]),
            Icon::Maximize => frame([0.0, 0.0], [9.0, 9.0], 2.0),
            Icon::Retry => {
                // Most of a circle, clockwise, ending in an arrowhead on the right.
                bow([0.0, 0.0], [6.0, 6.0], 4..=24);
                line([6.0, 0.0], [2.5, -3.0]);
                line([6.0, 0.0], [8.5, -3.5]);
            }
        }
    }
}

/// Draw a program's icon, as sharp as the screen allows. Where it is not at hand
/// a prompt stands in for it, in `ink`.
fn tile(painter: &egui::Painter, program: Program, left: Pos2, side: f32, ink: Color32) {
    let Some(drawing) = program.drawing() else {
        return Icon::Prompt.paint(painter, left + egui::vec2(side / 2.0, 0.0), ink);
    };
    let ctx = painter.ctx();
    let scale = ctx.pixels_per_point();
    let pixels = (side * scale).round() as usize;
    let key = egui::Id::unique((program.name(), pixels));
    let texture = ctx.data(|data| data.get_temp::<egui::TextureHandle>(key));
    let texture = texture.unwrap_or_else(|| {
        let image = raster(drawing, pixels);
        let texture = ctx.load_texture(program.name(), image, egui::TextureOptions::LINEAR);
        ctx.data_mut(|data| data.insert_temp(key, texture.clone()));
        texture
    });
    // One texel to a pixel: an icon that straddles pixels blurs.
    let corner = (left - egui::vec2(0.0, side / 2.0)).round_to_pixels(scale);
    let rect = Rect::from_min_size(corner, Vec2::splat(pixels as f32 / scale));
    let whole = Rect::from_min_max(Pos2::ZERO, egui::pos2(1.0, 1.0));
    painter.image(texture.id(), rect, whole, Color32::WHITE);
}

/// A program's icon with, on its corner, a dot in the colour of what the program
/// has to say, if it has anything: the two are then read as one. The icons of
/// those `behind` it, the other panes of its tab, show past its top and its
/// left, as a stack of three at most.
pub fn badge(
    painter: &egui::Painter,
    program: Program,
    behind: &[Program],
    left: Pos2,
    side: f32,
    ink: Color32,
    dot: Option<Color32>,
) {
    // Only what has an icon stacks: a prompt drawn in lines stands alone.
    let drawn = |program: &&Program| program.drawing().is_some();
    let most = if drawn(&&program) { 2 } else { 0 };
    let behind: Vec<_> = behind.iter().filter(drawn).take(most).collect();
    // Each stands a step up and to the left of the one before it, into the
    // row's own margin: the icon in front is where a lone one would be, and
    // as large.
    let layers = behind.len();
    for (back, program) in behind.into_iter().rev().chain([&program]).enumerate() {
        let left = left - Vec2::splat(side * 0.14 * (layers - back) as f32);
        if back > 0 {
            // A dark rim parts it from the one behind: the tile's own shape,
            // which is wider than it is tall, and a little more.
            let middle = left + egui::vec2(side / 2.0, 0.0);
            let rim = Rect::from_center_size(middle, side * egui::vec2(0.976, 0.76));
            painter.rect_filled(rim.expand(1.5), side * 0.2 + 1.5, Color32::from_black_alpha(170));
        }
        tile(painter, *program, left, side, ink);
    }
    if let Some(color) = dot {
        // A dark rim parts it from the icon, whatever that is drawn in.
        let at = left + egui::vec2(side - 2.0, side / 2.0 - 4.0);
        painter.circle_filled(at, 5.0, Color32::from_black_alpha(170));
        painter.circle_filled(at, 3.5, color);
    }
}

/// An icon in its own colours.
fn raster(drawing: &str, pixels: usize) -> egui::ColorImage {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_str(drawing, &usvg::Options::default());
    let tree = tree.expect("a bundled icon is sound");
    let side = pixels as u32;
    let mut canvas = tiny_skia::Pixmap::new(side, side).expect("an icon has a size");
    let scale = pixels as f32 / tree.size().width();
    let scale = tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, scale, &mut canvas.as_mut());
    let colors = canvas.pixels().iter().map(|pixel| {
        let rgba = [pixel.red(), pixel.green(), pixel.blue(), pixel.alpha()];
        Color32::from_rgba_premultiplied(rgba[0], rgba[1], rgba[2], rgba[3])
    });
    egui::ColorImage::new([pixels; 2], colors.collect())
}

/// The window's icon, for a desktop that takes it from the window. One that goes
/// by the desktop entry draws the same picture.
pub fn icon() -> egui::IconData {
    const SIDE: usize = 128;
    let image = raster(include_str!("../../assets/dev.rexy.client.svg"), SIDE);
    let rgba = image.pixels.iter().flat_map(Color32::to_srgba_unmultiplied);
    egui::IconData { rgba: rgba.collect(), width: SIDE as u32, height: SIDE as u32 }
}

/// A flat button that draws its own symbol.
pub fn icon_button(ui: &mut egui::Ui, icon: Icon, tip: &str) -> egui::Response {
    button(ui, 32.0, icon, tip, false)
}

/// One that turns something on and off, and shows which it is.
pub fn toggle(ui: &mut egui::Ui, icon: Icon, tip: &str, on: bool) -> egui::Response {
    button(ui, 32.0, icon, tip, on)
}

/// A lesser one, as small as those of a row.
pub fn small_button(ui: &mut egui::Ui, icon: Icon, tip: &str) -> egui::Response {
    button(ui, SMALL_BUTTON, icon, tip, false)
}

fn button(ui: &mut egui::Ui, side: f32, icon: Icon, tip: &str, on: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(side), Sense::click());
    if on {
        ui.painter().rect_filled(rect, RADIUS, ACTIVE);
    }
    paint_button(ui, rect, &response, icon);
    response.on_hover_text(tip)
}

fn paint_button(ui: &egui::Ui, rect: Rect, response: &egui::Response, icon: Icon) {
    let lit = eased(ui, response.id.with("over"), response.hovered());
    let fill = match response.is_pointer_button_down_on() {
        true => PRESSED,
        false => white((HOVER * lit) as u8),
    };
    ui.painter().rect_filled(rect, RADIUS, fill);
    let color = if ui.is_enabled() { DIM } else { white(56) };
    icon.paint(ui.painter(), rect.center(), color);
}

/// One of the window's own buttons: round, and always there to be seen.
pub fn round_button(ui: &mut egui::Ui, icon: Icon, tip: &str) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(Vec2::splat(SMALL_BUTTON), Sense::click());
    let lit = eased(ui, response.id.with("over"), response.hovered());
    let fill = match response.is_pointer_button_down_on() {
        true => PRESSED,
        false => white((20.0 + HOVER * lit) as u8),
    };
    ui.painter().circle_filled(rect.center(), SMALL_BUTTON / 2.0, fill);
    icon.paint(ui.painter(), rect.center(), white(205));
    response.on_hover_text(tip)
}

/// Text on one line, which ends in an ellipsis if it is too long for `width`.
pub fn fitted(
    ui: &egui::Ui,
    text: &str,
    font: FontId,
    color: Color32,
    width: f32,
) -> Arc<egui::Galley> {
    let mut job = LayoutJob::simple_singleline(text.to_owned(), font, color);
    job.wrap = TextWrapping::truncate_at_width(width);
    ui.painter().layout_job(job)
}

/// The word that a button asks with, and how wide that makes the button.
fn asks(ui: &egui::Ui, word: &str) -> (Arc<egui::Galley>, f32) {
    let word = ui.painter().layout_no_wrap(word.to_owned(), bold(SMALL), TEXT);
    let width = word.size().x + 2.0 * PILL;
    (word, width)
}

/// How wide the button is that asks with `word`.
pub fn confirm_width(ui: &egui::Ui, word: &str) -> f32 {
    asks(ui, word).1
}

/// A button that asks to be pressed, to close what a press before it would not,
/// lest programs end by a slip. Its trailing edge is at `edge`, where that press was.
pub fn confirm(ui: &egui::Ui, edge: Pos2, id: egui::Id, word: &str) -> egui::Response {
    let (word, width) = asks(ui, word);
    let size = egui::vec2(width, SMALL_BUTTON);
    let rect = Rect::from_min_size(edge - egui::vec2(size.x, size.y / 2.0), size);
    let response = ui.interact(rect, id, Sense::click());
    ui.painter().rect_filled(rect, RADIUS, DANGER);
    ui.painter().galley(rect.center() - word.size() / 2.0, word, TEXT);
    response
}

/// A sidebar row or a tab: flat, and filled when selected or under the pointer.
#[derive(Default)]
pub struct Item<'a> {
    pub label: &'a str,
    /// A second line, under its name, smaller and fainter. A row with one keeps
    /// its mark and its buttons beside the name.
    pub under: &'a str,
    /// A name of its own, for a row that must stay itself while the rows before
    /// it come and go: its menu and its buttons go by it.
    pub id: Option<egui::Id>,
    /// The icon of its program, ahead of the label.
    pub icon: Option<Program>,
    /// A picture in that place, for a row that is no program's.
    pub sign: Option<Icon>,
    /// The programs of the other panes it stands for, stacked behind that icon.
    pub behind: &'a [Program],
    pub selected: bool,
    /// A lesser row that heads the rows beneath it.
    pub caption: bool,
    /// It opens a list.
    pub menu: bool,
    /// Its name is in bold.
    pub strong: bool,
    /// A mark at its trailing edge.
    pub mark: Option<(Icon, Color32)>,
    /// What the mark means, to say in its place and its colour. Only a row whose
    /// label starts at its edge has the room.
    pub word: Option<&'a str>,
    /// The keys that do what the row does, each on a key of its own at the
    /// trailing edge, joined as Rex writes them: `Ctrl+Shift+D`.
    pub keys: Option<&'a str>,
    /// Small buttons that take the mark's place while the pointer is on the row.
    pub buttons: &'a [(Icon, &'a str)],
    /// The last of them was pressed, and asks with this word to be pressed again.
    pub confirm: Option<&'a str>,
    /// More to say of the row under the pointer, after its name if that was cut short.
    pub tip: &'a str,
    /// It can be dragged to another place among its like.
    pub movable: bool,
    /// It waits to be chosen a second time, to close what may be busy.
    pub danger: bool,
    /// How far in what it says starts, for a row that belongs to the one above.
    pub inset: f32,
}

/// Draw a row. Returns its response, and which of its buttons was pressed.
pub fn item(ui: &mut egui::Ui, size: Vec2, item: Item<'_>) -> (egui::Response, Option<usize>) {
    const BUTTON: f32 = SMALL_BUTTON;
    let sense = if item.movable { Sense::click_and_drag() } else { Sense::click() };
    let (rect, response) = match item.id {
        Some(id) => {
            let (_, rect) = ui.allocate_space(size);
            (rect, ui.interact(rect, id, sense))
        }
        None => ui.allocate_exact_size(size, sense),
    };
    // While anything is dragged, only it is lit, and no row offers its buttons.
    let dragging = ui.ctx().dragged_id().is_some();
    let over = response.dragged() || (!dragging && ui.rect_contains_pointer(rect));
    // One that cannot be taken now does not answer the pointer.
    let over = over && ui.is_enabled();
    // Its fill comes and goes over a moment.
    let lit = eased(ui, response.id.with("over"), over);
    let chosen = eased(ui, response.id.with("selected"), item.selected);
    let fill = match item.danger {
        true => DANGER,
        false => white((SELECTED * chosen).max(OVER * lit) as u8),
    };
    let color = match (item.selected || over || item.danger, item.caption) {
        (true, _) => TEXT,
        (_, true) => DIM,
        _ => white(190),
    };
    ui.painter().rect_filled(rect, RADIUS, fill);
    // The height its name is at: the row's middle, or above it for a second line.
    let lift = if item.under.is_empty() { 0.0 } else { LINE };
    let mid = rect.center().y - lift;
    // Room at the trailing edge is the label's to lose, so that it never moves.
    let word = item.word.map(|word| {
        // In the colour of the mark it goes with, where there is one.
        let (font, color) = (FontId::proportional(SMALL), item.mark.map_or(DIM, |mark| mark.1));
        ui.painter().layout_no_wrap(word.to_owned(), font, color)
    });
    let keys = caps(ui, item.keys.unwrap_or_default());
    // A dot sits on the corner of the row's icon, where it has one, and takes
    // no room of its own.
    let docked = item.icon.is_some() && matches!(item.mark, Some((Icon::Dot, _)));
    let aside = item.mark.filter(|_| !docked);
    let marks = f32::from(u8::from(aside.is_some()) + u8::from(item.menu));
    let marks = word.as_ref().map_or(14.0 * marks, |word| word.size().x + 8.0);
    let marks = marks + caps_width(&keys);
    // The button that asks is where the last one was, and goes by its name: the
    // pointer is on it already.
    let last = item.buttons.len().checked_sub(1);
    let asks = item.confirm.zip(last).map(|(word, last)| {
        let edge = egui::pos2(rect.right() - 4.0, mid);
        confirm(ui, edge, response.id.with(("button", last)), word)
    });
    let room = (12.0 + marks).max(4.0 + BUTTON * item.buttons.len() as f32);
    let room = asks.as_ref().map_or(room, |asks| room.max(asks.rect.width() + 10.0));
    let mut text = rect.shrink2(egui::vec2(12.0, 0.0));
    text.min.x += item.inset;
    text.max.x = rect.right() - room;
    // A row tall enough for two lines has the larger icon, whatever it says.
    let side = if size.y >= ROW { LARGE } else { TILE };
    let lead = if item.icon.is_some() || item.sign.is_some() { side + 8.0 } else { 0.0 };
    // A name too long for its row ends in an ellipsis.
    let font = match (item.caption, item.strong) {
        (true, _) => bold(SMALL),
        (_, true) => bold(BODY),
        _ => FontId::proportional(BODY),
    };
    let label = fitted(ui, item.label, font, color, text.width() - lead);
    let cut = label.elided;
    let needs = label.size() + egui::vec2(lead, 0.0);
    let place = Align2::LEFT_CENTER.align_size_within_rect(needs, text);
    let place = place.translate(egui::vec2(0.0, -lift));
    if let Some(program) = item.icon {
        let left = egui::pos2(place.left(), rect.center().y);
        let ink = if item.selected { tints(ui.ctx()).accent } else { DIM };
        let dot = item.mark.filter(|_| docked).map(|(_, color)| color);
        badge(ui.painter(), program, item.behind, left, side, ink, dot);
    } else if let Some(sign) = item.sign {
        let middle = egui::pos2(place.left() + side / 2.0, rect.center().y);
        sign.paint(ui.painter(), middle, if item.selected { TEXT } else { DIM });
    }
    let name = place.min + egui::vec2(lead, 0.0);
    // The second line starts under the name and has the row to its far edge.
    let under = (!item.under.is_empty()).then(|| {
        let font = FontId::proportional(SMALL);
        let under = fitted(ui, item.under, font, FAINT, rect.right() - 12.0 - name.x);
        let at = egui::pos2(name.x, rect.center().y + LINE - under.size().y / 2.0);
        ui.painter().galley(at, under.clone(), FAINT);
        under
    });
    let low = under.is_some_and(|under| under.elided);
    ui.painter().galley(name, label, color);
    let mut pressed = None;
    if let Some(asks) = asks {
        pressed = last.filter(|_| asks.clicked());
    } else if over && !dragging && !item.buttons.is_empty() {
        let mut right = rect.right() - 4.0;
        for (index, (icon, tip)) in item.buttons.iter().enumerate().rev() {
            let center = egui::pos2(right - BUTTON / 2.0, mid);
            let button = Rect::from_center_size(center, Vec2::splat(BUTTON));
            // By a name of its own: a row's tooltip goes by the row's and a number.
            let id = response.id.with(("button", index));
            let button_response = ui.interact(button, id, Sense::click());
            paint_button(ui, button, &button_response, *icon);
            if button_response.on_hover_text(*tip).clicked() {
                pressed = Some(index);
            }
            right -= BUTTON;
        }
    } else {
        let mut center = egui::pos2(rect.right() - 16.0, mid);
        if item.menu {
            Icon::Down.paint(ui.painter(), center, DIM);
            center.x -= 14.0;
        }
        if let Some(word) = word {
            let corner = egui::pos2(rect.right() - 12.0, mid) - word.size() * egui::vec2(1.0, 0.5);
            ui.painter().galley(corner, word, TEXT);
        } else if let Some((mark, color)) = aside {
            mark.paint(ui.painter(), center, color);
        }
        paint_caps(ui, keys, egui::pos2(rect.right() - 10.0, mid));
    }
    let response = if cut || low || !item.tip.is_empty() {
        response.on_hover_ui(|ui| {
            if cut {
                ui.label(item.label);
            }
            if low {
                ui.label(item.under);
            }
            if !item.tip.is_empty() {
                ui.label(item.tip);
            }
        })
    } else {
        response
    };
    (response, pressed)
}

/// The keys of a shortcut, each to go on a key of its own, as Rex joins them:
/// `Ctrl+Shift+D`.
fn caps(ui: &egui::Ui, keys: &str) -> Vec<Arc<egui::Galley>> {
    let mut names: Vec<_> = keys.split('+').filter(|key| !key.is_empty()).collect();
    // The plus key itself, which is also what joins the others.
    if keys.ends_with('+') {
        names.push("+");
    }
    let font = FontId::proportional(11.0);
    let key = |key: &str| ui.painter().layout_no_wrap(key.to_owned(), font.clone(), white(205));
    names.into_iter().map(key).collect()
}

/// The room a key takes.
fn cap(name: &egui::Galley) -> Vec2 {
    egui::vec2(name.size().x + 12.0, 18.0)
}

/// The room the keys of a shortcut take side by side, each with the gap after it.
fn caps_width(keys: &[Arc<egui::Galley>]) -> f32 {
    keys.iter().map(|name| cap(name).x + 4.0).sum()
}

/// Draw the keys of a shortcut, the last of them ending at `right`.
fn paint_caps(ui: &egui::Ui, keys: Vec<Arc<egui::Galley>>, mut right: Pos2) {
    for name in keys.into_iter().rev() {
        let size = cap(&name);
        let key = Rect::from_min_size(right - egui::vec2(size.x, 9.0), size);
        ui.painter().rect_filled(key, 5, white(22));
        ui.painter().galley(key.center() - name.size() / 2.0, name, TEXT);
        right.x -= size.x + 4.0;
    }
}

/// A key and what pressing it does, side by side: one of the things that can
/// be pressed next.
pub fn hint(ui: &mut egui::Ui, keys: &str, title: &str) {
    let keys = caps(ui, keys);
    let wide = caps_width(&keys);
    let title = hint_title(ui, title);
    let size = egui::vec2(wide + 4.0 + title.size().x, 22.0);
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    paint_caps(ui, keys, rect.left_center() + egui::vec2(wide - 4.0, 0.0));
    let at = rect.left_center() + egui::vec2(wide + 4.0, -title.size().y / 2.0);
    ui.painter().galley(at, title, TEXT);
}

/// How wide a [`hint`] of these `keys` and this `title` is.
pub fn hint_width(ui: &egui::Ui, keys: &str, title: &str) -> f32 {
    caps_width(&caps(ui, keys)) + 4.0 + hint_title(ui, title).size().x
}

/// What a hint says a key does, laid out as it is shown.
fn hint_title(ui: &egui::Ui, title: &str) -> Arc<egui::Galley> {
    ui.painter().layout_no_wrap(title.to_owned(), FontId::proportional(BODY), TEXT)
}

/// The mark in the margin of a row that heads others, which fold away under
/// it. It points at the row while they are `folded`, and down at them while
/// they are not and the pointer is on the row.
pub fn fold(ui: &egui::Ui, row: Rect, folded: bool) {
    if folded || ui.rect_contains_pointer(row) {
        let at = egui::pos2(row.left() + 5.0, row.center().y);
        let points = match folded {
            true => [[-1.5, -3.0], [1.5, 0.0], [-1.5, 3.0]],
            false => [[-3.0, -1.5], [0.0, 1.5], [3.0, -1.5]],
        };
        let points = points.map(|point| at + Vec2::from(point));
        ui.painter().line(points.to_vec(), Stroke::new(1.5, DIM));
    }
}

/// The mark ahead of a `row` that is one of several to choose: a box, with a
/// tick in it once the row is `chosen`.
pub fn tick(ui: &egui::Ui, row: Rect, chosen: bool) {
    let center = egui::pos2(row.left() + 20.0, row.center().y);
    let (fill, edge) = if chosen {
        (tints(ui.ctx()).accent, Color32::TRANSPARENT)
    } else {
        (Color32::TRANSPARENT, FAINT)
    };
    let square = Rect::from_center_size(center, Vec2::splat(14.0));
    ui.painter().rect(square, 4, fill, Stroke::new(1.5, edge), StrokeKind::Inside);
    if chosen {
        let points =
            [[-3.5, 0.0], [-1.0, 2.5], [3.5, -2.5]].map(|point| center + Vec2::from(point));
        ui.painter().line(points.to_vec(), Stroke::new(1.5, CARD));
    }
}

/// Words that are no row: what heads the rows under them, or stands where
/// there are none.
pub fn caption(ui: &mut egui::Ui, words: egui::RichText) {
    ui.add_space(8.0);
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.add(egui::Label::new(words.color(DIM)).selectable(false));
    });
    ui.add_space(2.0);
}

/// What opens the palette, for the pointer: as wide as the room there is, it
/// says what it is for, and the keys that do the same where they fit.
pub fn search(ui: &mut egui::Ui, keys: &str) -> egui::Response {
    let size = Vec2::new(ui.available_width(), 28.0);
    let (rect, response) = ui.allocate_exact_size(size, Sense::click());
    let lit = eased(ui, response.id.with("over"), response.hovered());
    ui.painter().rect_filled(rect, RADIUS, white((OVER + OVER * lit) as u8));
    let left = rect.left_center();
    Icon::Search.paint(ui.painter(), left + egui::vec2(16.0, 0.0), DIM);
    let font = FontId::proportional(BODY);
    let word = ui.painter().layout_no_wrap("Search".to_owned(), font, DIM);
    let keys = caps(ui, keys);
    if 32.0 + word.size().x + 12.0 + caps_width(&keys) <= rect.width() {
        paint_caps(ui, keys, rect.right_center() - egui::vec2(5.0, 0.0));
    }
    let at = left + egui::vec2(32.0, -word.size().y / 2.0);
    ui.painter().galley(at, word, DIM);
    response
}

/// An entry of a menu: a row as wide as the menu, with the keys that do the same
/// at its far edge. One that cannot be taken now is faint.
pub fn entry(ui: &mut egui::Ui, enabled: bool, row: Item<'_>) -> egui::Response {
    let size = Vec2::new(ui.available_width(), 30.0);
    ui.add_enabled_ui(enabled, |ui| item(ui, size, row).0).inner
}

/// The one prominent action of a page or dialog.
pub fn suggested(label: &str) -> egui::Button<'_> {
    egui::Button::new(egui::RichText::new(label).font(bold(BODY)).color(TEXT)).fill(ACCENT_FILL)
}

#[cfg(test)]
#[path = "../../tests/unit/ui_style.rs"]
mod tests;
