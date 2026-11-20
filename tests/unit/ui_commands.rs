use super::*;
use crate::model::{Pane, Session};
use std::collections::HashSet;

fn pane(id: &str) -> Pane {
    Pane { id: id.into(), title: id.to_uppercase(), idle: id == "idle", ..Pane::default() }
}

fn tab(id: &str, panes: &[&str]) -> Tab {
    Tab { id: id.into(), panes: panes.iter().map(|id| pane(id)).collect(), ..Tab::default() }
}

/// One session on show, with three tabs: the second on show, and split.
fn view() -> View {
    let session =
        |id: &str, tabs| Session { id: id.into(), name: id.to_uppercase(), active: None, tabs };
    let tabs = vec![tab("t1", &["a"]), tab("t2", &["b", "idle", "c"]), tab("t3", &["d"])];
    View {
        connected: true,
        sessions: vec![session("s1", tabs), session("s2", Vec::new())],
        selected_session: Some("s1".into()),
        selected_tab: Some("t2".into()),
        ..Default::default()
    }
}

/// How a window stands with nothing wanting the user and no tab to go back to.
fn standing<'a>(view: &'a View, keys: &'a Keys) -> Standing<'a> {
    Standing {
        view,
        focused: None,
        font_size: 15.0,
        sidebar: false,
        keys,
        wanting: &[],
        peeked: None,
        back: None,
        removable: false,
        marking: false,
    }
}

/// What a command comes to with the keyboard in a pane, in words.
fn plan(view: &View, focused: &str, name: &str, args: Value) -> Result<String, &'static str> {
    let keys = Keys::default();
    let standing = Standing { focused: Some(focused), ..standing(view, &keys) };
    Ok(match standing.plan(name, &args, None)? {
        Deed::Server(Action::SelectTab { tab, .. }) => format!("show {tab}"),
        Deed::Server(Action::SelectSession(session)) => format!("show {session}"),
        Deed::Server(Action::Edit { edit, .. }) => format!("{edit:?}"),
        Deed::Server(Action::Split { pane, toward }) => format!("split {pane} {toward:?}"),
        Deed::Server(Action::Swap { pane, with }) => format!("swap {pane} {with}"),
        Deed::Server(Action::Zoom { pane }) => format!("zoom {pane}"),
        Deed::Server(Action::MovePane { pane, tab }) => format!("move {pane} to {tab:?}"),
        Deed::Server(Action::Key { code, .. }) => format!("press {code}"),
        Deed::Server(Action::NewFloat { tab, .. }) => format!("float over {tab}"),
        Deed::Server(Action::Stow { layers, back, .. }) => format!("stow {layers:?} for {back:?}"),
        Deed::Server(Action::Float { pane, .. }) => format!("float {pane}"),
        Deed::Server(Action::Dock { pane, beside }) => format!("dock {pane} by {beside}"),
        Deed::Server(Action::Place { layer, bounds, .. }) => format!("{layer} at {bounds:?}"),
        Deed::Focus(pane) => format!("focus {pane}"),
        Deed::Close(_, spared, _) => format!("close, spared: {spared}"),
        Deed::CloseTab(_, tab, spared) => format!("close {tab}, spared: {spared}"),
        Deed::Rename(_, tab, name) => format!("rename {tab:?} from {name}"),
        Deed::Font(size) => format!("font {size}"),
        _ => "other".into(),
    })
}

#[test]
fn the_table_names_each_command_once_and_spells_its_keys_as_rex_does() {
    let names: HashSet<_> = COMMANDS.iter().map(|command| command.0).collect();
    assert_eq!(names.len(), COMMANDS.len());
    let mut seen = HashSet::new();
    for (key, name) in KEYS {
        assert!(seen.insert(key), "{key} is bound twice");
        assert!(names.contains(name), "{key} is bound to {name}, which is no command");
        // What a keyboard cannot be heard to press would never be matched. A
        // plus is the one key whose name is also what joins a chord.
        let heard = *key == "ctrl++" || input::press("p", key).is_some();
        assert!(heard, "{key} is not a key as Rex spells it");
    }
    // Those of the mode that selects by the keyboard, likewise, but for the
    // mode's own: a sequence's keys are parted by spaces.
    let mut seen = HashSet::new();
    for (keys, name) in SELECT_KEYS {
        assert!(seen.insert(keys), "{keys} is bound twice in the mode");
        assert!(names.contains(name), "{keys} is bound to {name}, which is no command");
        for key in keys.split(' ') {
            assert!(input::press("p", key).is_some(), "{key} is not a key as Rex spells it");
        }
    }
    // Every command of the caret's is bound in it.
    let caret = COMMANDS.iter().filter(|command| mark(command.0).is_some());
    for (name, ..) in caret {
        assert!(SELECT_KEYS.iter().any(|bound| bound.1 == *name), "{name} has no key");
    }
    // A server is told of them all, and of the names that take a direction.
    let request = request();
    let count = |list: &Value| list.as_array().unwrap().len();
    assert_eq!(count(&request["actions"]), COMMANDS.len() + DIRECTED.len());
    // With a key for each of nine tabs, the last of which is the last tab.
    let bindings = &request["defaults"][0]["bindings"];
    assert_eq!(count(bindings), KEYS.len() + 9 + SELECT_KEYS.len());
    let gg = bindings.as_array().unwrap().iter().find(|bound| bound["keys"] == json!(["g", "g"]));
    assert_eq!(
        gg.cloned().unwrap(),
        json!({"keys": ["g", "g"], "action": "client.select.top", "mode": "select"})
    );
    let bound = |key: &str| {
        let has = |bound: &&Value| bound["keys"] == json!([key]);
        bindings.as_array().unwrap().iter().find(has).cloned().unwrap()
    };
    assert_eq!(
        bound("alt+3"),
        json!({"keys": ["alt+3"], "action": "client.tab.goto", "args": {"index": 3}})
    );
    assert_eq!(bound("alt+9")["args"], json!({"index": -1}));
    assert_eq!(
        bound("ctrl+shift+t"),
        json!({"keys": ["ctrl+shift+t"], "action": "client.tab.new"})
    );
    assert!(input::press("p", "alt+9").is_some());
}

#[test]
fn the_next_pane_that_wants_the_user_is_looked_at_from_here_unless_it_is_here() {
    let (view, keys) = (view(), Keys::default());
    // The first is in the tab on show. The others are elsewhere.
    let wanting = [(0, "c".to_owned()), (2, "x".to_owned()), (1, "y".to_owned())];
    let look = |focused, peeked, wanting| {
        let standing = Standing { focused, peeked, wanting, ..standing(&view, &keys) };
        match standing.plan(peek::NEXT, &Value::Null, None) {
            Ok(Deed::Reveal(_, pane)) => format!("go to {pane}"),
            Ok(Deed::Peek(server, pane)) => format!("look at {pane} on {server}"),
            _ => "nothing".into(),
        }
    };
    // With none looked at, the one after the pane with the keyboard: which is
    // gone to where it is on show.
    assert_eq!(look(Some("b"), None, &wanting), "go to c");
    assert_eq!(look(Some("c"), None, &wanting), "look at x on 2");
    // From one looked at, the one after it, round and round.
    assert_eq!(look(Some("c"), Some("x"), &wanting), "look at y on 1");
    assert_eq!(look(Some("c"), Some("y"), &wanting), "go to c");
    assert_eq!(look(Some("b"), Some("y"), &wanting[1..]), "look at x on 2");
    // One that wants nothing more gives way to the first that does.
    assert_eq!(look(Some("b"), Some("gone"), &wanting[1..]), "look at x on 2");
    // With no other there is nothing to do.
    assert_eq!(look(Some("b"), Some("x"), &wanting[1..2]), "nothing");
    assert_eq!(look(Some("c"), None, &wanting[..1]), "nothing");
    assert_eq!(look(None, None, &[]), "nothing");
}

#[test]
fn the_next_pane_that_wants_the_user_is_the_one_after_this_on_any_server() {
    let (view, keys) = (view(), Keys::default());
    let wanting = [(0, "a".to_owned()), (2, "x".to_owned()), (1, "y".to_owned())];
    let next = |focused, wanting| {
        let standing = Standing { focused: Some(focused), wanting, ..standing(&view, &keys) };
        match standing.plan("client.activity.next", &Value::Null, None) {
            Ok(Deed::Reveal(server, pane)) => Some((server, pane)),
            _ => None,
        }
    };
    // From a pane that wants nothing, the first. From one that does, the one
    // after it, and round to the first again.
    assert_eq!(next("b", &wanting), Some((0, "a".to_owned())));
    assert_eq!(next("a", &wanting), Some((2, "x".to_owned())));
    assert_eq!(next("y", &wanting), Some((0, "a".to_owned())));
    // The pane with the keyboard is not gone to, and there may be no other.
    assert_eq!(next("a", &wanting[..1]), None);
    assert_eq!(next("b", &[]), None);
}

#[test]
fn the_tab_shown_before_is_gone_back_to_on_whichever_server_it_is() {
    let (view, keys) = (view(), Keys::default());
    let back = ((3, "s7".to_owned()), "t9".to_owned());
    let plan = |back| {
        Standing { back, ..standing(&view, &keys) }.plan("client.tab.back", &Value::Null, None)
    };
    let there = |deed: &Deed| matches!(deed, Deed::Show(3, Action::SelectTab { session, tab }) if session == "s7" && tab == "t9");
    assert!(plan(Some(&back)).is_ok_and(|deed| there(&deed)));
    assert!(matches!(plan(None), Err("refused")));
}

#[test]
fn an_action_with_a_direction_goes_by_a_name_of_its_own() {
    let right = json!({"direction": "right"});
    assert_eq!(named("pane.split", &right), "pane.split.right");
    assert_eq!(
        named("client.tab.move", &json!({"direction": "forward"})),
        "client.tab.move.forward"
    );
    assert_eq!(named("pane.split.right", &right), "pane.split.right");
    assert_eq!(named("pane.zoom", &right), "pane.zoom");
    let mut view = view();
    let split = |view: &View, way: &str| plan(view, "b", "pane.split", json!({"direction": way}));
    assert_eq!(split(&view, "right").as_deref(), Ok("split b [1.0, 0.0]"));
    assert_eq!(split(&view, "up").as_deref(), Ok("split b [0.0, -1.0]"));
    // One this client lacks is not among its actions.
    assert_eq!(split(&view, "auto"), Err("not_supported"));
    // A pane changes places with the one on that side of it, where there is one.
    let panes = &mut view.sessions[0].tabs[1].panes;
    for (at, pane) in panes.iter_mut().enumerate() {
        (pane.x, pane.width, pane.height) = (at as f32 / 3.0, 1.0 / 3.0, 1.0);
    }
    let moved = |way: &str| plan(&view, "b", "pane.move", json!({"direction": way}));
    assert_eq!(moved("right").as_deref(), Ok("swap b idle"));
    assert_eq!(moved("left"), Err("refused"));
}

#[test]
fn a_command_is_planned_for_what_is_on_show() {
    let view = view();
    let plan = |name, args| plan(&view, "b", name, args);
    let none = Value::Null;
    assert_eq!(plan("client.settings", none.clone()), Err("not_supported"));
    assert_eq!(plan("client.font.increase", none.clone()).as_deref(), Ok("font 16"));
    // Tabs go round and round, and by number from either end.
    assert_eq!(plan("client.tab.next", none.clone()).as_deref(), Ok("show t3"));
    assert_eq!(plan("client.tab.previous", none.clone()).as_deref(), Ok("show t1"));
    let index = |index: i64| plan("client.tab.goto", json!({"index": index}));
    assert_eq!(index(1).as_deref(), Ok("show t1"));
    assert_eq!(index(-1).as_deref(), Ok("show t3"));
    assert_eq!((index(0), index(4), index(-4)), (Err("refused"), Err("refused"), Err("refused")));
    assert_eq!(plan("client.tab.goto", none.clone()), Err("refused"));
    // A tab moves to before the one before it, or to after the one after it.
    let moved = |name| plan(name, Value::Null);
    assert_eq!(moved("client.tab.move.backward").as_deref(), Ok("MoveBefore(Some(\"t1\"))"));
    assert_eq!(moved("client.tab.move.forward").as_deref(), Ok("MoveBefore(None)"));
    assert_eq!(plan("session.next", none.clone()).as_deref(), Ok("show s2"));
    let select = |id: &str| plan("session.select", json!({"session_id": id}));
    assert_eq!((select("s2").as_deref(), select("s9")), (Ok("show s2"), Err("refused")));
    // A session's name is there to be changed only in the sidebar.
    assert_eq!(plan("session.rename", none.clone()), Err("refused"));
    // A tab nobody named has no name to start from: it goes by its terminal's title.
    assert_eq!(plan("client.tab.rename", none.clone()).as_deref(), Ok("rename Some(\"t2\") from "));
    assert_eq!(plan("client.tab.close", none.clone()).as_deref(), Ok("close t2, spared: false"));
    // Panes are those of the tab on show, round and round.
    assert_eq!(plan("pane.focus_next", none.clone()).as_deref(), Ok("focus idle"));
    assert_eq!(plan("pane.focus_previous", none.clone()).as_deref(), Ok("focus c"));
    assert_eq!(plan("pane.zoom", none.clone()).as_deref(), Ok("zoom b"));
    assert_eq!(plan(TO_TAB, none.clone()).as_deref(), Ok("move b to None"));
    assert_eq!(plan("pane.close", none.clone()).as_deref(), Ok("close, spared: false"));
    assert_eq!(plan("pane.send_key", json!({"key": "ctrl+b"})).as_deref(), Ok("press key_b"));
    assert_eq!(plan("pane.send_key", none.clone()), Err("refused"));
    // No history is kept of a terminal that has yet to show anything.
    assert_eq!(plan("client.scroll.page_up", none.clone()), Err("refused"));
    assert_eq!(plan("client.mode.exit", none), Err("refused"));
}

#[test]
fn a_command_is_refused_where_it_has_nothing_to_act_on() {
    let mut view = view();
    // A pane another client names is one on show, or none.
    let keys = Keys::default();
    let standing = |view| Standing { focused: Some("b"), sidebar: true, ..standing(view, &keys) };
    let named = |pane| standing(&view).plan("pane.close", &Value::Null, Some(pane));
    assert!(matches!(named("idle"), Ok(Deed::Close(_, true, _))));
    assert!(matches!(named("a"), Err("refused")));
    let rename = standing(&view).plan("session.rename", &Value::Null, None);
    assert!(matches!(rename, Ok(Deed::Rename(..))));
    // What is pasted goes to the pane named. Where nothing is selected there is
    // nothing to copy.
    let paste = standing(&view).plan("pane.paste", &Value::Null, Some("c"));
    assert!(matches!(paste, Ok(Deed::Paste(pane)) if pane == "c"));
    let copy = standing(&view).plan("pane.copy", &Value::Null, None);
    assert!(matches!(copy, Err("refused")));
    // A key bound to what cannot be done is still the window's, unless a
    // program is to have it: one that moves through history where there is none.
    let keeps = |name| standing(&view).keeps(name, &Value::Null);
    assert!(keeps("client.mode.exit") && keeps("client.tab.goto") && keeps("pane.zoom"));
    assert!(keeps("pane.copy"));
    assert!(!keeps("client.scroll.page_up") && !keeps("client.settings"));
    // A tab of one pane has nothing to zoom, and a first tab nowhere to move back
    // to. Its pane has a tab of its own already.
    view.selected_tab = Some("t1".into());
    assert_eq!(plan(&view, "a", "pane.zoom", Value::Null), Err("refused"));
    assert_eq!(plan(&view, "a", TO_TAB, Value::Null), Err("refused"));
    assert_eq!(plan(&view, "a", "pane.focus_next", Value::Null), Err("refused"));
    assert_eq!(plan(&view, "a", "client.tab.move.backward", Value::Null), Err("refused"));
    assert_eq!(plan(&view, "b", "pane.split.right", Value::Null), Err("refused"));
    view.selected_tab = Some("t3".into());
    assert_eq!(plan(&view, "d", "client.tab.move.forward", Value::Null), Err("refused"));
    // Without a connection there is the window itself to change, and the connection.
    view.connected = false;
    assert_eq!(plan(&view, "d", "client.tab.new", Value::Null), Err("refused"));
    assert_eq!(plan(&view, "d", "client.font.reset", Value::Null).as_deref(), Ok("font 15"));
    assert!(plan(&view, "d", "session.refresh", Value::Null).is_ok());
}

#[test]
fn other_clients_may_only_have_a_session_shown_until_they_are_let_do_more() {
    let off = false;
    assert!(allowed("session.select", off) && !allowed("pane.paste", off));
    // Nor may they let themselves.
    assert!(!allowed(REMOTE, off) && allowed("pane.paste", true));
    // The palette says what choosing it does, as things stand.
    assert_eq!(label(REMOTE, title(REMOTE), off), "Allow Remote Control");
    assert_eq!(label(REMOTE, title(REMOTE), true), "Refuse Remote Control");
    assert_eq!(label("pane.zoom", title("pane.zoom"), true), "Zoom Pane");
}

#[test]
fn hosts_are_added_always_and_removed_while_one_can_be() {
    let (view, keys) = (View::default(), Keys::default());
    let opens = |removable: bool, name: &str| {
        let standing = Standing { removable, ..standing(&view, &keys) };
        match standing.plan(name, &Value::Null, None) {
            Ok(Deed::Palette(listing)) => Some(listing),
            _ => None,
        }
    };
    // Home need not be there to be asked: it answers when the host is typed.
    assert!(opens(false, hosts::ADD) == Some(Listing::Host(None)));
    assert!(opens(true, hosts::REMOVE) == Some(Listing::Hosts));
    assert!(opens(false, hosts::REMOVE).is_none());
}

#[test]
fn floating_panes_are_made_put_away_and_brought_back_by_one_command() {
    let mut view = view();
    let said = |view: &View, focused: &str, name: &str| {
        plan(view, focused, name, Value::Null).unwrap_or_else(|_| "nothing".into())
    };
    // With none, one is made: and a pane of the layout can be made to float.
    assert_eq!(said(&view, "b", floats::TOGGLE), "float over t2");
    assert_eq!(said(&view, "b", floats::FLOAT), "float b");
    // With one on show, the command goes to it from a pane of the layout, and
    // from the one that floats puts it away: and that pane can go back into
    // the layout.
    let tab = &mut view.sessions[0].tabs[1];
    tab.panes[2].float = Some("f".into());
    tab.floats = vec![Float { id: "f".into(), ..Float::default() }];
    assert_eq!(said(&view, "b", floats::TOGGLE), "stow [] for Some(\"c\")");
    assert_eq!(said(&view, "c", floats::TOGGLE), "stow [\"f\"] for None");
    assert_eq!(said(&view, "c", floats::FLOAT), "dock c by b");
    // Of panes with more room than others, it goes back beside the largest.
    let idle = &mut view.sessions[0].tabs[1].panes[1];
    (idle.width, idle.height) = (0.5, 1.0);
    assert_eq!(said(&view, "c", floats::FLOAT), "dock c by idle");
    assert_eq!(said(&view, "b", floats::NEW), "float over t2");
    // Put away, it is brought back for its pane.
    let tab = &mut view.sessions[0].tabs[1];
    tab.floats[0].away = true;
    let away = tab.panes.pop().unwrap();
    tab.hidden.push(away);
    assert_eq!(said(&view, "b", floats::TOGGLE), "stow [\"f\"] for Some(\"c\")");
    // Behind a pane that fills the tab, it is gone to as it is from beside it.
    let tab = &mut view.sessions[0].tabs[1];
    (tab.floats[0].away, tab.zoomed) = (false, true);
    assert_eq!(said(&view, "b", floats::TOGGLE), "stow [] for Some(\"c\")");
    // A tab keeps a pane on show: the last pane of a layout stays in it, and
    // where all of them float none is put away. One of those is given the tab,
    // there being no layout for it to go back into.
    let tab = &mut view.sessions[0].tabs[1];
    tab.zoomed = false;
    tab.panes = tab.hidden.split_off(0);
    assert_eq!(said(&view, "c", floats::TOGGLE), "nothing");
    assert_eq!(said(&view, "c", floats::FLOAT), "f at [0.0, 0.0, 1.0, 1.0]");
    view.sessions[0].tabs[1].panes[0].float = None;
    assert_eq!(said(&view, "c", floats::FLOAT), "nothing");
}

#[test]
fn selecting_by_the_keyboard_is_the_windows_own_and_never_goes_to_a_server() {
    let view = view();
    let keys = Keys::default();
    let plan = |keys: &Keys, marking: bool, name: &str| {
        let standing = Standing { focused: Some("b"), marking, ..standing(&view, keys) };
        standing.plan(name, &Value::Null, None).map(|deed| matches!(deed, Deed::Mark(_)))
    };
    // A caret's keys are refused while there is none.
    assert_eq!(plan(&keys, false, "client.select.down"), Err("refused"));
    assert_eq!(plan(&keys, true, "client.select.down"), Ok(true));
    // And beginning is refused where the keymap has no mode to select in.
    let mut bare = Keys::default();
    bare.map = std::sync::Arc::new(crate::ui::keymap::Keymap::default());
    assert_eq!(plan(&bare, false, SELECT), Err("refused"));
}
