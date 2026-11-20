//! The keys that are the window's own, by the keymap in force, and those that go
//! to the terminal. Other clients' requests are answered here too, and what
//! home's actions ask of the window's other servers is passed on.
use eframe::egui::{self, Align2, Key, Margin, Modifiers, Popup, Rect};
use serde_json::Value;

use super::{
    App,
    commands::{self, SELECT_MODE},
    input,
    keymap::{self, Binding, FORWARD, Step},
    palette,
    style::{BODY, DIM, SMALL, bold, hint, hint_width},
};
use crate::{
    model::{Action, Called, Pane, View},
    terminal::{Mark, Request},
};

/// How many keys are said to be pressable next after part of a sequence.
const HINTS: usize = 18;

impl App {
    /// While part of a sequence of keys has been pressed, or a mode is entered,
    /// say so at the foot of the terminals, which are `within`: the mode by its
    /// name, and the keys that can be pressed next with what each does.
    ///
    /// A mode is something to work in, and may bind any number of keys, so it
    /// keeps to one line and says how many more it binds. The palette lists
    /// them all. What follows part of a sequence is a choice to make now, and
    /// wraps onto more.
    pub(super) fn hints(&self, ctx: &egui::Context, within: Rect) {
        let keys = &self.keys;
        if keys.pending.is_empty() && keys.modes.is_empty() {
            return;
        }
        let next = keys.map.next(&keys.modes, &keys.pending);
        let mode = keys.modes.last().map(|(name, _)| name.as_str());
        let frame = egui::Frame::popup(&ctx.global_style()).inner_margin(Margin::symmetric(12, 8));
        let area = egui::Area::new(egui::Id::unique("key hints"))
            .order(egui::Order::Foreground)
            .interactable(false)
            .pivot(Align2::CENTER_BOTTOM)
            .fixed_pos(within.center_bottom() - egui::vec2(0.0, 14.0));
        let wide = (within.width() - 56.0).clamp(160.0, 760.0);
        let line = keys.pending.is_empty();
        let more = |left: usize| {
            let more = format!("and {left} more");
            egui::Label::new(egui::RichText::new(more).size(SMALL).color(DIM)).selectable(false)
        };
        area.show(ctx, |ui| {
            frame.show(ui, |ui| {
                ui.set_max_width(wide);
                let show = |ui: &mut egui::Ui| {
                    let gap = 14.0;
                    ui.spacing_mut().item_spacing = egui::vec2(gap, 6.0);
                    if let Some(mode) = mode {
                        let name = egui::RichText::new(mode).font(bold(BODY));
                        ui.add(egui::Label::new(name).selectable(false));
                    }
                    // On one line, room is kept for saying how many are left.
                    let room = ui.available_width() - 90.0;
                    let mut used = 0.0;
                    let mut shown = 0;
                    for (key, title) in next.iter().take(HINTS) {
                        let key = keymap::shown(key);
                        if line {
                            used += hint_width(ui, &key, title) + gap;
                            if used > room && shown > 0 {
                                break;
                            }
                        }
                        hint(ui, &key, title);
                        shown += 1;
                    }
                    if next.len() > shown {
                        ui.add(more(next.len() - shown));
                    }
                };
                match line {
                    true => ui.horizontal(show),
                    false => ui.horizontal_wrapped(show),
                };
            });
        });
    }

    /// Take this frame's key presses that the keymap binds, and do what they are
    /// bound to, on the server whose `view` is on show. What is left is the
    /// terminal's. The keymap is `home`'s, as Rex has it: a device follows its
    /// home for its keys, wherever the session it shows is.
    pub(super) fn shortcuts(&mut self, ctx: &egui::Context, home: &View, view: &View) {
        let pressed = |modifiers, key| ctx.input_mut(|i| i.consume_key(modifiers, key));
        // A terminal's programs own Ctrl+C, X and V. In a text field they mean
        // what they usually do.
        if ctx.text_edit_focused() {
            for (key, event) in [(Key::C, egui::Event::Copy), (Key::X, egui::Event::Cut)] {
                if pressed(Modifiers::CTRL, key) {
                    ctx.input_mut(|i| i.events.push(event));
                }
            }
            if pressed(Modifiers::CTRL, Key::V) {
                self.paste(ctx, false);
            }
        }
        // It holds while home is there. Without home the keys are this client's own.
        self.keys.follow(home.keymap.as_ref().filter(|_| home.connected));
        self.keyboard.prefix = None;
        let press = |(index, event): (usize, &egui::Event)| match *event {
            egui::Event::Key { key, physical_key, pressed: true, repeat, modifiers } => {
                Some((index, key, physical_key, repeat, modifiers))
            }
            _ => None,
        };
        let presses: Vec<_> =
            ctx.input(|i| i.events.iter().enumerate().filter_map(press).collect());
        let mut taken = Vec::new();
        for (index, key, physical, repeat, modifiers) in presses {
            let modifiers = self.keyboard.bits(modifiers);
            let spelled = input::spellings(key, physical, modifiers);
            let key = physical.unwrap_or(key);
            if spelled.is_some_and(|spelled| self.press(ctx, view, key, repeat, spelled)) {
                taken.push(index);
            }
        }
        if !taken.is_empty() {
            ctx.input_mut(|i| input::remove(&mut i.events, &taken));
        }
    }

    /// Take a key press, spelled as Rex spells it, and say whether it was the
    /// window's: if not, it is left to whatever has the keyboard.
    fn press(
        &mut self,
        ctx: &egui::Context,
        view: &View,
        key: Key,
        repeat: bool,
        spelled: Vec<String>,
    ) -> bool {
        let map = self.keys.map.clone();
        let chord = spelled[0].clone();
        // A key held down from a press that was the window's does again only what
        // is the better for it, and nothing else: its repeats would answer for
        // the user, or put away the palette it opened.
        let held = self.keys.held.as_ref();
        if let Some((_, again)) = held.filter(|held| repeat && held.0 == chord).cloned() {
            if let Some(again) = again {
                self.run(ctx, view, &again.name, &again.args, "key");
            }
            return true;
        }
        let again = |bound: &Binding| map.repeats(&bound.name).then(|| bound.clone());
        // A text field keeps its keys, but for those that are about the field
        // itself: what puts the palette away, and what finds the next match.
        if self.busy(ctx) {
            let palette = Popup::is_id_open(ctx, palette::id());
            let about = |bound: &&Binding| {
                let puts_away = ["client.palette.toggle", "session.switch"].contains(&&*bound.name);
                let finds = self.find.is_some() && bound.name.starts_with("client.find.");
                ((palette && puts_away) || finds)
                    && bound.mode.is_none()
                    && matches!(&bound.keys[..], [key] if spelled.contains(key))
            };
            let bound = map.bindings.iter().find(about);
            if let Some(bound) = bound {
                self.run(ctx, view, &bound.name, &bound.args, "key");
            }
            self.keys.held = bound.map(|bound| (chord, again(bound)));
            return bound.is_some();
        }
        let mut pending = std::mem::take(&mut self.keys.pending);
        pending.push(spelled);
        // A key that a function on the server answers to can always be sent there.
        let can = |bound: &Binding| {
            let own = self.standing(view).keeps(&bound.name, &bound.args);
            own || bound.name == FORWARD || map.served(&bound.name).is_some()
        };
        let step = keymap::press(&map, &self.keys.modes, &mut pending, can);
        self.keys.pending = pending;
        let (mut taken, mut repeated) = (true, None);
        match step {
            Step::Pass => taken = false,
            Step::Wait | Step::Void => {}
            Step::Leave => _ = self.keys.modes.pop(),
            Step::Run(bound) => {
                // A mode entered for one key has had it.
                let spent =
                    |(name, once): &(String, bool)| *once && Some(name) == bound.mode.as_ref();
                self.keys.modes.retain(|mode| !spent(mode));
                if bound.name == FORWARD {
                    // The key goes there as any key does, with the keys before it,
                    // and marked as one a function there answers.
                    if let [prefix @ .., _] = bound.keys.as_slice() {
                        self.keyboard.prefix = Some((key, prefix.to_vec()));
                    }
                    taken = false;
                } else {
                    self.run(ctx, view, &bound.name, &bound.args, "key");
                    repeated = again(bound);
                }
            }
        }
        self.keys.held = taken.then_some((chord, repeated));
        taken
    }

    /// Do something by its name, for the key or the row of the palette that asked:
    /// an action of this client's, or else one of the server's own.
    pub(super) fn run(
        &mut self,
        ctx: &egui::Context,
        view: &View,
        name: &str,
        args: &Value,
        origin: &'static str,
    ) {
        let plan = self.standing(view).plan(name, args, None);
        // A look at another pane is over when anything else is asked for, done
        // or not: unless to look at the next, or to paste, which is typing.
        let looking = matches!(plan, Ok(commands::Deed::Peek(..) | commands::Deed::Paste(_)));
        match plan {
            Ok(deed) => self.act(ctx, self.current, deed, false),
            Err("not_supported") => self.perform(view, name, args, origin, self.focused.clone()),
            Err(_) => {}
        }
        if !looking {
            self.unpeek();
        }
    }

    /// Have a server do one of its own actions, to a pane of the server on
    /// show if one is given. A terminal's is that server's to do. One of the
    /// configuration's is home's, whichever server is on show, as the keys
    /// are: home is told the window's other servers by name, for the action
    /// to call through this client, and which of them the session on show is
    /// on, where it is not home's own.
    fn perform(
        &mut self,
        view: &View,
        name: &str,
        args: &Value,
        origin: &'static str,
        pane: Option<String>,
    ) {
        let (name, args) = (name.to_owned(), args.clone());
        // A terminal's is named for what makes it, with dots: none of the
        // configuration's has a dot in its name.
        if name.contains('.') {
            let (servers, far) = (Vec::new(), None);
            return self.send(Action::Perform { name, args, origin, pane, servers, far });
        }
        let named = |server: &super::Server| server.label().to_owned();
        let servers = self.servers.iter().skip(1).map(named).collect();
        let far = (self.current != 0).then(|| named(&self.servers[self.current]));
        let far = far.zip(view.selected_session.clone());
        self.tell(0, Action::Perform { name, args, origin, pane, servers, far });
    }

    /// Pass on what each server has for another: the calls that an action of
    /// home's has this client make there, and to home what came of each. Only
    /// home was told of the others, so only home's calls are made. An action
    /// may go on long after the user began it here, so this is done whether
    /// or not the window can be seen.
    pub(super) fn relay(&mut self) {
        for from in 0..self.servers.len() {
            let (calls, called) = self.servers[from].client.relays();
            for call in calls {
                let named = |to: &&super::Server| from == 0 && to.label() == call.server;
                let to = self.servers.iter().skip(1).find(named);
                let (asked, to) = (call.call.clone(), to.ok_or("Rexy has no such server".into()));
                if let Err(error) = to.and_then(|to| to.client.try_send(Action::Call(call))) {
                    let answer = Err(["refused".to_owned(), error]);
                    self.tell(from, Action::Called(Called { call: asked, answer }));
                }
            }
            for called in called {
                self.tell(0, Action::Called(called));
            }
        }
    }

    /// Do what the buttons and menus asked for while they were drawn, and what
    /// was asked of this client from each server: by another client, which is
    /// answered, or by an action of the server's that this client began.
    pub(super) fn answer(&mut self, ctx: &egui::Context, views: &[View]) {
        for (name, pane) in std::mem::take(&mut self.todo) {
            let view = &views[self.current];
            match self.standing(view).plan(name, &Value::Null, pane.as_deref()) {
                Ok(deed) => self.act(ctx, self.current, deed, true),
                Err("not_supported") => self.perform(view, name, &Value::Null, "palette", pane),
                Err(_) => {}
            }
        }
        for (server, view) in views.iter().enumerate() {
            for request in &view.asked {
                // What an action this client began asks in its turn is done
                // where the user is: home's are done for other servers too.
                let server = if request.call.is_some() { server } else { self.current };
                let view = &views[server];
                // A server's clients have no say over what another server's session
                // shows, nor over a session that is not on show: but to have one shown.
                let shown = request.session.is_none() || request.session == view.selected_session;
                let ours = server == self.current && shown;
                // What another client asks waits on an answer, and is done only
                // by the user's leave. What an action this client began asks in
                // its turn is the user's own doing.
                let remote = self.remembered.kept.remote;
                let barred = request.call.is_some() && !commands::allowed(&request.name, remote);
                let plan = if barred {
                    Err(commands::BARRED)
                } else if request.name == "session.select" || ours {
                    let (name, pane) = (&request.name, request.pane.as_deref());
                    self.standing(view).plan(name, &request.args, pane)
                } else {
                    Err("refused")
                };
                let error = plan.as_ref().err().copied();
                // What asks the user first is answered now, not when the user decides.
                if let Ok(deed) = plan {
                    self.act(ctx, server, deed, false);
                }
                if let Some(call) = request.call.clone() {
                    self.tell(server, Action::Reply { call, error });
                }
            }
        }
    }

    /// Keep the caret that selects by the keyboard with its mode: shown in the
    /// pane the keys go to while the mode is entered, and put away when it is
    /// left. Keys that go to another pane leave it, and so does a pane with
    /// nothing on screen yet to select. The last screen of a connection lost
    /// can be selected from, as it can be copied.
    pub(super) fn marks(&mut self, views: &[View]) {
        let entered = self.keys.modes.iter().any(|(name, _)| name == SELECT_MODE);
        let typing = self.typing().map(|(server, pane)| (server, pane.to_owned()));
        let leave = |app: &mut Self| app.keys.modes.retain(|(name, _)| name != SELECT_MODE);
        if let Some((server, pane)) = self.marking.clone()
            && (!entered || typing.as_ref() != Some(&(server, pane.clone())))
        {
            self.ask(server, &pane, Request::Mark(Mark::End));
            self.marking = None;
            return leave(self);
        }
        if !entered || self.marking.is_some() {
            return;
        }
        let shown =
            |(server, pane): &(usize, String)| views.get(*server)?.find(pane)?.2.frame.as_ref();
        match typing.filter(|typing| shown(typing).is_some()) {
            Some(_) => self.begin_marking(),
            None => leave(self),
        }
    }

    /// Show the caret in the pane that has the keyboard, if one does.
    pub(super) fn begin_marking(&mut self) {
        if let Some((server, pane)) = self.typing().map(|(server, pane)| (server, pane.to_owned()))
        {
            self.ask(server, &pane, Request::Mark(Mark::Begin));
            self.marking = Some((server, pane));
        }
    }

    /// Send this frame's keyboard events to the pane of a server's that has
    /// the keyboard.
    pub(super) fn terminal_keys(&mut self, ctx: &egui::Context, server: usize, pane: &Pane) {
        let selected = pane.frame.as_ref().is_some_and(|f| !f.selected.is_empty());
        // Escape leaves a selection instead of reaching the program.
        let left = selected && ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
        let actions = ctx.input(|i| self.keyboard.translate(&pane.id, &i.events));
        // Typing lets go of one too. The pane's worker is not woken for nothing:
        // this is asked at every key typed.
        if selected && (left || actions.iter().any(input::clears_selection)) {
            self.ask(server, &pane.id, Request::Deselect);
        }
        for action in actions {
            self.tell(server, action);
        }
    }
}
