//! What each key does: the keymap of the server on show, as Rex resolved it from
//! this client's own keys and the account's `init.lua`, or without one this
//! client's own keys alone. Sequences and modes are run here, as Rex leaves
//! them to its clients.
use std::sync::Arc;

use serde_json::{Value, json};

use super::{commands, input};

/// The action of a key that a function on the server is bound to: the key is
/// sent there, as any key is.
pub const FORWARD: &str = "client.key.forward";

fn items(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

/// A key as it is shown: `ctrl+shift+d` is `Ctrl+Shift+D`.
pub fn shown(key: &str) -> String {
    let word = |word: &str| match word.trim_matches(['[', ']']) {
        "pageup" => "PageUp".to_owned(),
        "pagedown" => "PageDown".to_owned(),
        word => {
            let first = word.chars().take(1).flat_map(char::to_uppercase);
            first.chain(word.chars().skip(1)).collect()
        }
    };
    key.split('+').map(word).collect::<Vec<_>>().join("+")
}

#[derive(Clone, Debug, PartialEq)]
pub struct Binding {
    /// One key as Rex spells it, or the keys of a sequence in order.
    pub keys: Vec<String>,
    /// The mode it belongs to, if it is not for all the time.
    pub mode: Option<String>,
    pub name: String,
    pub args: Value,
}

struct Mode {
    name: String,
    /// The keys it does not bind do nothing while it is entered.
    exclusive: bool,
    /// Keys that do nothing while it is entered, whatever is bound to them outside it.
    blocked: Vec<Vec<String>>,
    /// Keys it does not bind are the window's, if the window binds them, and
    /// otherwise do nothing: none reaches the terminal while it is entered.
    /// Selecting with the keyboard is such a mode, as Rex has no word for it.
    sealed: bool,
}

#[derive(Default)]
pub struct Keymap {
    /// In the order to try them.
    pub bindings: Vec<Binding>,
    modes: Vec<Mode>,
    /// The server's own actions: each by its name, its title if it is one to
    /// show, and whether holding its key does it again.
    pub served: Vec<(String, String, bool)>,
}

impl Keymap {
    /// Read a server's answer to `client.keymap`.
    pub fn parse(reply: &Value) -> Self {
        let text = |value: &Value| value.as_str().map(str::to_owned);
        let keys = |value: &Value| items(value).filter_map(text).collect::<Vec<_>>();
        let binding = |bound: &Value| {
            Some(Binding {
                keys: keys(&bound["keys"]),
                mode: text(&bound["mode"]),
                name: commands::named(bound["action"].as_str()?, &bound["args"]),
                args: bound["args"].clone(),
            })
        };
        let mode = |mode: &Value| {
            let name = text(&mode["name"])?;
            Some(Mode {
                sealed: name == commands::SELECT_MODE,
                name,
                exclusive: mode["exclusive"] == true,
                blocked: items(&mode["blocked"]).map(keys).collect(),
            })
        };
        let served = |action: &Value| {
            let title = text(&action["title"]).unwrap_or_default();
            Some((text(&action["name"])?, title, action["repeats"] == true))
        };
        Self {
            bindings: items(&reply["bindings"]).filter_map(binding).collect(),
            modes: items(&reply["modes"]).filter_map(mode).collect(),
            served: items(&reply["actions"]).filter_map(served).collect(),
        }
    }

    /// This client's own keys, which are all there is until a server has answered:
    /// those a server is asked with, read as its answer is. A server tries the
    /// later of one layer's bindings first, and so does this.
    /// Its modes are those its bindings name, as a server has them.
    pub fn own() -> Self {
        let mut own = Self::parse(&commands::request()["defaults"][0]);
        own.bindings.reverse();
        let mut named: Vec<String> = own.bindings.iter().filter_map(|b| b.mode.clone()).collect();
        named.sort();
        named.dedup();
        let modes = named.into_iter().map(|name| json!({"name": name})).collect::<Vec<_>>();
        own.modes = Self::parse(&json!({"modes": modes})).modes;
        own
    }

    pub fn has_mode(&self, name: &str) -> bool {
        self.modes.iter().any(|mode| mode.name == name)
    }

    /// Whether a mode of those entered lets no key reach the terminal.
    fn sealed(&self, modes: &[(String, bool)]) -> bool {
        let sealed = |name: &String| self.modes.iter().any(|m| m.sealed && &m.name == name);
        modes.iter().any(|(name, _)| sealed(name))
    }

    /// One of the server's own actions, by its name.
    pub fn served(&self, name: &str) -> Option<&(String, String, bool)> {
        self.served.iter().find(|served| served.0 == name)
    }

    /// Whether holding the key of an action does it again.
    pub fn repeats(&self, name: &str) -> bool {
        let own = commands::command(name).map(|own| own.2);
        own.or(self.served(name).map(|served| served.2)) == Some(true)
    }

    /// The keys of a binding that is `wanted`, as they are shown: `Ctrl+Shift+D`,
    /// or nothing where there is none. Of several, those that would be tried
    /// first. Keys that never come to it are not its keys: those that something
    /// tried before it always takes, as what is sent on to the server does.
    fn keys(&self, wanted: impl Fn(&Binding) -> bool) -> String {
        let always = |over: &Binding| over.name == FORWARD || self.served(&over.name).is_some();
        let reached = |at: usize, bound: &Binding| {
            let over = |over: &Binding| over.mode.is_none() && over.keys == bound.keys;
            !self.bindings[..at].iter().any(|b| over(b) && always(b))
        };
        let mut bound = self.bindings.iter().enumerate();
        let bound =
            bound.find(|(at, bound)| wanted(bound) && bound.mode.is_none() && reached(*at, bound));
        bound.map_or_else(String::new, |(_, bound)| {
            let keys = bound.keys.iter().map(|key| shown(key));
            keys.collect::<Vec<_>>().join(", ")
        })
    }

    /// The keys that do something by its name.
    pub fn shortcut(&self, name: &str) -> String {
        self.keys(|bound| bound.name == name)
    }

    /// The keys that show the tab at a place among so many: those for its
    /// place from the first, which is 1, or else from the last, which is -1.
    pub fn tab(&self, place: usize, of: usize) -> String {
        let at = |index: i64| {
            self.keys(|bound| bound.name == "client.tab.goto" && bound.args["index"] == index)
        };
        let keys = at(place as i64 + 1);
        if keys.is_empty() { at(place as i64 - of as i64) } else { keys }
    }

    /// What an action is called where it is shown: by this client's table, by the
    /// server whose action it is, or failing both by its name. A mode is
    /// entered by its own name.
    pub fn title(&self, bound: &Binding) -> String {
        match (bound.name.as_str(), bound.args["name"].as_str()) {
            ("client.mode.enter", Some(mode)) => format!("{mode}…"),
            (FORWARD, _) => "Send to Terminal".to_owned(),
            (name, _) => {
                let served = self.served(name).map_or("", |served| &served.1);
                let titles = [commands::title(name), served, name];
                titles.into_iter().find(|title| !title.is_empty()).unwrap_or_default().to_owned()
            }
        }
    }

    /// What can be pressed next, and what each key would do: after the keys
    /// `pressed` so far of a sequence, each in the ways Rex spells it, or with
    /// none pressed in the last of the `modes` entered. Of several bindings for
    /// a key, the one that would be tried first.
    pub fn next(&self, modes: &[(String, bool)], pressed: &[Vec<String>]) -> Vec<(&str, String)> {
        let entered = |mode: &str| modes.iter().any(|(name, _)| name == mode);
        let last = modes.last().map(|(name, _)| name.as_str());
        let goes_on = |bound: &&Binding| {
            let follows = bound.keys.len() > pressed.len() && fits(pressed, &bound.keys);
            // With nothing pressed, only what the mode itself binds is worth saying.
            follows
                && match pressed.is_empty() {
                    true => bound.mode.as_deref() == last,
                    false => bound.mode.as_deref().is_none_or(entered),
                }
        };
        let mut next: Vec<(&str, String)> = Vec::new();
        for bound in self.bindings.iter().filter(goes_on) {
            let key = bound.keys[pressed.len()].as_str();
            if !next.iter().any(|(seen, _)| *seen == key) {
                // More keys follow it.
                let more = bound.keys.len() > pressed.len() + 1;
                let title = self.title(bound);
                next.push((key, if more { format!("{title}…") } else { title }));
            }
        }
        next
    }

    /// What a control says it does under the pointer, with the keys that do the same.
    pub fn tip(&self, label: &str, name: &str) -> String {
        match self.shortcut(name).as_str() {
            "" => label.to_owned(),
            keys => format!("{label} ({keys})"),
        }
    }
}

/// What a key press comes to.
#[derive(Debug, PartialEq)]
pub enum Step<'a> {
    /// Nothing: it is the terminal's.
    Pass,
    /// A sequence may go on from it.
    Wait,
    /// Nothing, and it goes nowhere: a mode has it ignored, or it was Escape
    /// in the middle of a sequence.
    Void,
    /// The last mode entered is left.
    Leave,
    Run(&'a Binding),
}

/// Whether the keys `pressed`, each in the ways Rex spells it, are a binding's
/// keys or the first of them.
fn fits(pressed: &[Vec<String>], keys: &[String]) -> bool {
    let spelled = |(pressed, key): (&Vec<String>, _)| pressed.contains(key);
    keys.len() >= pressed.len() && pressed.iter().zip(keys).all(spelled)
}

/// What the last key pressed comes to. `pressed` ends with it, after the keys of
/// the sequence it may go on, each in the ways Rex spells it. `modes` are those
/// entered, the last of them the first to be asked. Of several bindings for a
/// key, the first whose action `can` be done is the one.
pub fn resolve<'a>(
    map: &'a Keymap,
    modes: &[(String, bool)],
    pressed: &[Vec<String>],
    can: impl Fn(&Binding) -> bool,
) -> Step<'a> {
    let escape = pressed.last().is_some_and(|key| key.iter().any(|key| key == input::ESCAPE));
    if escape && pressed.len() > 1 {
        return Step::Void;
    }
    let fits = |keys: &[String]| fits(pressed, keys);
    let first = |mode: Option<&str>| {
        let mut bound = map.bindings.iter();
        let bound = bound.find(|b| b.mode.as_deref() == mode && fits(&b.keys) && can(b))?;
        Some(match bound.keys.len() > pressed.len() {
            true => Step::Wait,
            false => Step::Run(bound),
        })
    };
    for (name, _) in modes.iter().rev() {
        if let Some(step) = first(Some(name)) {
            return step;
        }
        // Escape leaves a mode that has no other use for it.
        if escape {
            return Step::Leave;
        }
        let mode = map.modes.iter().find(|mode| &mode.name == name);
        if mode.is_some_and(|mode| mode.exclusive || mode.blocked.iter().any(|keys| fits(keys))) {
            return Step::Void;
        }
    }
    first(None).unwrap_or(Step::Pass)
}

/// The same of the last of the keys `pending`, which are left as the keys the
/// next one comes after. One that goes on no sequence is taken as itself, and
/// the keys before it count for nothing: it may be bound alone, or begin a
/// sequence of its own.
pub fn press<'a>(
    map: &'a Keymap,
    modes: &[(String, bool)],
    pending: &mut Vec<Vec<String>>,
    can: impl Fn(&Binding) -> bool,
) -> Step<'a> {
    let mut step = resolve(map, modes, pending, &can);
    if step == Step::Pass && pending.len() > 1 {
        pending.drain(..pending.len() - 1);
        step = resolve(map, modes, pending, can);
    }
    if step == Step::Pass && map.sealed(modes) {
        step = Step::Void;
    }
    if step != Step::Wait {
        pending.clear();
    }
    step
}

/// Where the keyboard stands with the keymap in force.
pub struct Keys {
    /// The server's answer the keymap was read from, or none for this client's own.
    reply: Option<Arc<Value>>,
    pub map: Arc<Keymap>,
    /// The modes entered, each with whether one key is all it was entered for.
    pub modes: Vec<(String, bool)>,
    /// The keys pressed so far of a sequence, each in the ways Rex spells it.
    pub pending: Vec<Vec<String>>,
    /// The key last taken from the terminal, as it is spelled, and what holding
    /// it does again.
    pub held: Option<(String, Option<Binding>)>,
}

impl Default for Keys {
    fn default() -> Self {
        Self::of(None)
    }
}

impl Keys {
    fn of(reply: Option<&Arc<Value>>) -> Self {
        Self {
            reply: reply.cloned(),
            map: Arc::new(reply.map_or_else(Keymap::own, |reply| Keymap::parse(reply))),
            modes: Vec::new(),
            pending: Vec::new(),
            held: None,
        }
    }

    /// Go by a server's answer, or by none. Modes and a sequence under way belong
    /// to the keymap they began in, and end with it.
    pub fn follow(&mut self, reply: Option<&Arc<Value>>) {
        if reply.map(Arc::as_ptr) != self.reply.as_ref().map(Arc::as_ptr) {
            *self = Self::of(reply);
        }
    }
}

#[cfg(test)]
#[path = "../../tests/unit/ui_keymap.rs"]
mod tests;
