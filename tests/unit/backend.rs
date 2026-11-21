use super::*;

/// A layout node as the server sends one: where it sits, and what is in it.
fn leaf(rect: [f32; 4], block: &str) -> Value {
    json!({"rect": rect, "block_id": block})
}

fn split(rect: [f32; 4], before: Value, after: Value) -> Value {
    json!({"rect": rect, "split": {"before": before, "after": after}})
}

/// One pane down the left, and two on the right, one above the other.
fn three() -> Value {
    let mut right = split(
        [0.6, 0., 0.4, 1.],
        leaf([0.6, 0., 0.4, 0.25], "b"),
        leaf([0.6, 0.25, 0.4, 0.75], "c"),
    );
    right["split"]["direction"] = json!("vertical");
    let mut layout = split(FULL, leaf([0., 0., 0.6, 1.], "a"), right);
    layout["split"]["direction"] = json!("horizontal");
    layout
}

fn tabs(view: &Value) -> Vec<Tab> {
    parse_tabs(view, "me", &mut Vec::new())
}

#[test]
fn panes_sit_where_the_server_puts_them() {
    let view = json!({"windows": [{"window_id": "w", "layers": [{"layout": three()}]}]});
    let tab = &tabs(&view)[0];
    let placed = |pane: &Pane| (pane.id.clone(), [pane.x, pane.y, pane.width, pane.height]);
    let panes: Vec<_> = tab.panes.iter().map(placed).collect();
    let at = |id: &str, rect| (id.to_owned(), rect);
    assert_eq!(
        panes,
        [
            at("a", [0., 0., 0.6, 1.]),
            at("b", [0.6, 0., 0.4, 0.25]),
            at("c", [0.6, 0.25, 0.4, 0.75])
        ]
    );
}

#[test]
fn every_split_of_a_layer_on_show_has_a_border_to_drag() {
    let tiled = json!({"kind": "tiled", "layout": three()});
    let tab = |layers: Value| json!({"windows": [{"window_id": "w", "layers": layers}]});
    let borders = |view: &Value| tabs(view).remove(0).borders;
    let border = |before: &str, after: &str, upright, room, at| Border {
        before: before.into(),
        after: after.into(),
        upright,
        room,
        at,
        float: None,
    };
    // Each is named by the first pane of either side, inner splits first.
    assert_eq!(
        borders(&tab(json!([tiled]))),
        [border("b", "c", false, [0.6, 0., 0.4, 1.], 0.25), border("a", "b", true, FULL, 0.6)]
    );
    // A floating layer has its own, after the tab's, which keeps them under one.
    let bounds = [0.2, 0.2, 0.5, 0.5];
    let floating = |state: &str| {
        let layout =
            split(bounds, leaf([0.2, 0.2, 0.25, 0.5], "d"), leaf([0.45, 0.2, 0.25, 0.5], "e"));
        json!({"layer_id": "f", "kind": "floating", "state": state, "bounds": bounds, "layout": layout})
    };
    let over = tabs(&tab(json!([tiled, floating("normal")]))).remove(0);
    let within = Border { float: Some("f".into()), ..border("d", "e", true, bounds, 0.45) };
    assert_eq!(over.borders[2..], [within]);
    // Its panes are on show after the tab's own, and know the layer they are in.
    let float = Float { id: "f".into(), bounds, away: false };
    assert_eq!(over.floats, std::slice::from_ref(&float));
    let layers = |panes: &[Pane]| panes.iter().map(|pane| pane.float.clone()).collect::<Vec<_>>();
    let f = || Some("f".to_owned());
    assert_eq!(layers(&over.panes), [None, None, None, f(), f()]);
    // Put away, the layer is still the tab's, and its panes are out of sight.
    let away = tabs(&tab(json!([tiled, floating("minimized")]))).remove(0);
    assert_eq!(away.floats, [Float { away: true, ..float }]);
    assert_eq!((away.borders.len(), layers(&away.hidden)), (2, vec![f(), f()]));
    // Nor has a tab that one pane fills.
    let mut zoomed = tab(json!([tiled]));
    zoomed["windows"][0]["zoomed_block_id"] = json!("b");
    assert_eq!(borders(&zoomed), []);
}

#[test]
fn what_is_not_a_terminal_keeps_its_place_and_says_what_it_is() {
    let blocks = json!([
        {"block_id": "t", "creator_name": TERMINAL, "label": "shell"},
        {"block_id": "o", "creator_name": "com.superlogical.tunnel"},
    ]);
    let layout = split(FULL, leaf([0., 0., 0.5, 1.], "t"), leaf([0.5, 0., 0.5, 1.], "o"));
    let layers = json!([{"layout": layout, "blocks": blocks}]);
    let view = json!({"windows": [{"window_id": "w", "layers": layers}]});
    let mut terminals = Vec::new();
    let tabs = parse_tabs(&view, "me", &mut terminals);
    let [terminal, other] = &tabs[0].panes[..] else {
        panic!("a pane was left out");
    };
    assert_eq!(terminals, ["t"]);
    assert_eq!((terminal.title.as_str(), other.x), ("shell", 0.5));
    assert_eq!(other.title, "com.superlogical.tunnel");
    assert_eq!(other.status, "com.superlogical.tunnel: not a terminal");
}

#[test]
fn the_most_pressing_of_what_programs_report_speaks_for_their_terminal() {
    let none = (Activity::Idle, "", None);
    fn reported(records: &Value) -> (Activity, &str, Option<Program>) {
        let (activity, record, app) = super::reported(items(records));
        (activity, record["msg"].as_str().unwrap_or_default(), app)
    }
    let unheard = json!([{"state": "unheard-of"}]);
    assert_eq!(reported(&json!([])), none);
    assert_eq!(reported(&Value::Null), none);
    assert_eq!(reported(&unheard), none);
    let records = json!([
        {"state": "working", "effective_app": "claude", "msg": "Thinking"},
        {"state": "blocked", "kind": "permission", "msg": "Run it?"},
        {"state": "blocked", "kind": "permission", "msg": "And this?"},
        {"state": "done", "effective_app": "cargo"},
    ]);
    let (activity, message, app) = reported(&records);
    assert_eq!(activity, Activity::Blocked(Wait::Approval));
    // Of two as pressing, the first.
    assert_eq!(message, "Run it?");
    // One that has finished lends its icon no longer.
    assert_eq!(app, Program::named(["claude"]));
    let waits = [("question", Wait::Question), ("auth", Wait::SignIn), ("unheard-of", Wait::Input)];
    for (kind, wait) in waits {
        let records = json!([{"state": "blocked", "kind": kind}]);
        assert_eq!(reported(&records).0, Activity::Blocked(wait));
    }
    // What a program left behind is still to be looked at, failure before success.
    let left = json!([{"state": "done", "msg": "Built"}, {"state": "error", "msg": "Broke"}]);
    assert_eq!(reported(&left), (Activity::Error, "Broke", None));
}

#[test]
fn zoom_and_minimized_layers_are_respected() {
    let view = json!({"windows": [{"window_id": "w", "zoomed_block_id": "b", "layers": [
        {
            "layout": leaf([0.2, 0.2, 0.5, 0.5], "b"),
            "blocks": [{"block_id": "b", "owner": "me"}],
        },
        {"state": "minimized", "layout": leaf(FULL, "hidden")},
    ]}]});
    let tabs = tabs(&view);
    assert_eq!(tabs[0].panes.len(), 1);
    assert_eq!(tabs[0].panes[0].width, 1.);
    assert!(tabs[0].panes[0].owner);
    // The panes of a layer that was put away are kept track of, out of sight, as
    // are those behind a zoomed one.
    let ids = |panes: &[Pane]| panes.iter().map(|p| p.id.clone()).collect::<Vec<_>>();
    assert_eq!(ids(&tabs[0].hidden), ["hidden"]);
    let layout = split(FULL, leaf([0., 0., 0.5, 1.], "a"), leaf([0.5, 0., 0.5, 1.], "b"));
    let tab = json!({"window_id": "w", "zoomed_block_id": "b", "layers": [{"layout": layout}]});
    let tabs = self::tabs(&json!({"windows": [tab]}));
    assert_eq!(ids(&tabs[0].panes), ["b"]);
    assert_eq!(ids(&tabs[0].hidden), ["a"]);
    assert!(tabs[0].zoomed && tabs[0].split());
}

#[test]
fn a_view_is_asked_for_only_when_a_newer_one_is_heard_of() {
    let mut held = Held::default();
    // With none held, one is asked for, and once only while it is on its way.
    assert!(held.want(0));
    assert!(!held.want(0) && !held.want(3));
    // The one that comes is older than the news that came meanwhile: ask again.
    assert!(held.take(2));
    held.asking = false;
    assert!(!held.current() && held.want(0));
    assert!(held.take(3));
    held.asking = false;
    // Held, so neither old news nor an answer naming it asks again. Newer does.
    assert!(held.current());
    assert!(!held.want(2) && !held.want(3));
    assert!(held.want(4));
    // Views come in any order, and an older one is not taken over a newer.
    assert!(held.take(5));
    assert!(!held.take(4) && !held.take(5));
    assert_eq!(held.revision, Some(5));
}

#[test]
fn the_list_of_sessions_follows_the_news_of_it() {
    let mut sessions = Vec::new();
    let mut told = |event: Event| {
        // Told twice, as when news of a change comes after an answer that had it.
        list(&mut sessions, &event);
        list(&mut sessions, &event);
        let ids = sessions.iter().map(|session| session.id.clone());
        ids.collect::<Vec<_>>().join(" ")
    };
    let created = |session: &str| Event::Created { session: session.into(), name: String::new() };
    let moved = |session: &str, before: Option<&str>| Event::Moved {
        session: session.into(),
        before: before.map(str::to_owned),
    };
    told(created("a"));
    told(created("b"));
    assert_eq!(told(created("c")), "a b c");
    assert_eq!(told(moved("c", Some("a"))), "c a b");
    assert_eq!(told(moved("c", None)), "a b c");
    assert_eq!(told(moved("a", Some("c"))), "b a c");
    // News of what is not there changes nothing.
    assert_eq!(told(moved("gone", Some("b"))), "b a c");
    assert_eq!(told(moved("b", Some("gone"))), "a c b");
    let session = "c".to_owned();
    assert_eq!(told(Event::Destroyed { session }), "a b");
    assert_eq!(sessions[0].name, "Untitled");
}

#[test]
fn a_terminal_is_what_the_news_of_it_last_said() {
    let mut block = Block::default();
    block.note(News::Title("vim".into()));
    let (program, pid) = (Program::named(["claude"]), Some(7));
    block.note(News::Program(wire::Running { program, pid, ..Default::default() }));
    assert_eq!((block.title.as_str(), block.running.pid), ("vim", Some(7)));
    let report = |block: &mut Block, id: &str, record: Option<Value>| {
        let before = block.says().0;
        block.note(News::Status(vec![(id.into(), record)]));
        block.news(before)
    };
    // What a program comes to is news once, and its parts speak after it.
    let working = json!({"state": "working", "effective_app": "cargo"});
    assert_eq!(report(&mut block, "", Some(working.clone())), None);
    let asks = json!({"state": "blocked", "kind": "question", "msg": "Which?"});
    let asked = Some("Question: Which?".to_owned());
    assert_eq!(report(&mut block, "build/test", Some(asks.clone())), asked);
    assert_eq!(report(&mut block, "build/test", Some(asks)), None);
    assert_eq!(block.says().2, Program::named(["cargo"]));
    // A record replaces the one of its name, and one cleared says nothing more.
    let done = json!({"state": "done"});
    assert_eq!(report(&mut block, "", Some(done)), None);
    assert_eq!(report(&mut block, "build/test", None), Some("Done".into()));
    assert_eq!(report(&mut block, "", None), None);
    assert_eq!(block.says(), (Activity::Idle, "", None));
}

#[test]
fn a_pane_is_given_its_shells_directory_at_its_prompt_and_what_its_program_is_at() {
    let block = Block {
        title: "ada@studio: /home/ada/code".into(),
        shell: Some(7),
        running: wire::Running { pid: Some(7), ..Default::default() },
        prompt: "zsh".into(),
        home: "/home/ada".into(),
        ..Block::default()
    };
    let mut blocks = HashMap::from([("p".to_owned(), block)]);
    let pane = |blocks: &HashMap<String, Block>| {
        let held = Pane { id: "p".into(), title: "Terminal".into(), ..Pane::default() };
        let tab = Tab { panes: vec![held], ..Tab::default() };
        let mut sessions = vec![Session { tabs: vec![tab], ..Session::default() }];
        fill(&mut sessions, blocks);
        sessions.remove(0).tabs.remove(0).panes.remove(0)
    };
    let note = |blocks: &mut HashMap<String, Block>, news| blocks.get_mut("p").unwrap().note(news);
    // With no directory known, the title its prompt set.
    assert_eq!(pane(&blocks).title, "ada@studio: /home/ada/code");
    // The server says where the process in the foreground is: no shell has to.
    let shell = json!({"pid": 7, "name": "zsh", "argv0": "-zsh", "cwd": "/home/ada/code"});
    note(&mut blocks, News::Program(wire::running(&shell)));
    assert_eq!(pane(&blocks).title, "zsh ~/code");
    // A program in the foreground has a title of its own to go by. One whose
    // directory cannot be read leaves the terminal where it was known to be.
    let less = json!({"pid": 9, "name": "less", "argv0": "/usr/bin/less"});
    note(&mut blocks, News::Program(wire::running(&less)));
    assert_eq!(pane(&blocks).title, "ada@studio: /home/ada/code");
    assert_eq!(blocks["p"].running.folder.as_deref(), Some("/home/ada/code"));
    // What it is at and how far along, by the record that speaks for it, or
    // failing that as the terminal itself was told.
    note(&mut blocks, News::Progress(Some(10)));
    let status = |record| News::Status(vec![(String::new(), Some(record))]);
    let working = json!({"state": "working", "title": "Refactor auth", "progress": 60});
    note(&mut blocks, status(working));
    let held = pane(&blocks);
    assert_eq!((held.task.as_str(), held.progress), ("Refactor auth", Some(60)));
    note(&mut blocks, status(json!({"state": "working"})));
    let held = pane(&blocks);
    assert_eq!((held.task.as_str(), held.progress), ("", Some(10)));
    // A program that gives itself no title goes by its own name, whatever the
    // pane went by before it: not by the prompt it has left. A terminal with
    // nothing known in its foreground goes by the pane's label.
    let mut sessions = vec![Session {
        tabs: vec![Tab {
            panes: vec![Pane {
                id: "p".into(),
                title: "zsh ~/code".into(),
                label: "shell".into(),
                ..Pane::default()
            }],
            ..Tab::default()
        }],
        ..Session::default()
    }];
    note(&mut blocks, News::Title(String::new()));
    fill(&mut sessions, &blocks);
    assert_eq!(sessions[0].tabs[0].panes[0].title, "less");
    note(&mut blocks, News::Program(wire::running(&Value::Null)));
    fill(&mut sessions, &blocks);
    assert_eq!(sessions[0].tabs[0].panes[0].title, "shell");
}

#[test]
fn what_keeps_failing_is_tried_again_less_often_or_not_at_all() {
    let lost = anyhow::Error::from(Ended::Dropped);
    let secs = |failures, login| retry(&lost, failures, login).map(|wait| wait.as_secs());
    // A server reached without a login, and a terminal of a session joined, for ever.
    let open: Vec<_> = (0..5).map(|failures| secs(failures, false)).collect();
    assert_eq!(open, [2, 15, 15, 15, 15].map(Some));
    // One that is logged in to three times, and then when the user says.
    let login: Vec<_> = (0..4).map(|failures| secs(failures, true)).collect();
    assert_eq!(login, [Some(2), Some(15), None, None]);
    // A server that this client cannot talk to is not asked again, however it is
    // reached, nor a terminal that has closed.
    let refused = anyhow::Error::from(Ended::Incompatible("update the server".into()));
    assert_eq!(retry(&refused.context("Connecting"), 0, false), None);
    assert_eq!(retry(&Ended::BlockClosed.into(), 0, false), None);
}

#[test]
fn control_and_release_keys_never_duplicate_text() {
    assert_eq!(key_payload("c", "key_c", model::CTRL, "press")["text"], "");
    assert_eq!(key_payload("A", "key_a", model::SHIFT, "press")["text"], "A");
    assert_eq!(key_payload("a", "key_a", 0, "release")["text"], "");
    assert_eq!(key_payload("a", "key_a", 0, "repeat")["text"], "a");
    assert_eq!(key_payload("a", "key_a", 0, "repeat")["action"], "repeat");
}

#[test]
fn shift_is_used_up_only_in_typing_something_else() {
    let shift = model::SHIFT;
    let consumed = |key, code| key_payload(key, code, shift, "press")["consumed_mods"].clone();
    assert_eq!(consumed("A", "key_a"), "shift");
    assert_eq!(consumed("!", "digit_1"), "shift");
    // Or a program could not tell these from Tab, Enter and Space.
    assert_eq!(consumed("\t", "tab"), "");
    assert_eq!(consumed("\n", "enter"), "");
    assert_eq!(consumed(" ", "space"), "");
    assert_eq!(key_payload("\t", "tab", shift, "press")["mods"], "shift");
    assert_eq!(key_payload("a", "key_a", 0, "press")["consumed_mods"], "");
}

#[test]
fn a_key_is_known_by_what_it_types_without_shift() {
    let (shift, ctrl) = (model::SHIFT, model::CTRL);
    let unshifted = |key, code, bits| {
        let key = key_payload(key, code, bits, "press");
        key["unshifted_codepoint"].as_u64().and_then(|c| char::from_u32(c as u32))
    };
    assert_eq!(unshifted("n", "key_n", 0), Some('n'));
    assert_eq!(unshifted("N", "key_n", shift), Some('n'));
    assert_eq!(unshifted("!", "digit_1", shift), Some('1'));
    assert_eq!(unshifted("?", "slash", shift | ctrl), Some('/'));
    assert_eq!(unshifted("\t", "tab", shift), Some('\t'));
    // A letter follows the keyboard's layout, with Shift or without.
    assert_eq!(unshifted("a", "key_q", ctrl), Some('a'));
    assert_eq!(unshifted("É", "key_e", shift), Some('é'));
    assert_eq!(unshifted("é", "unidentified", 0), Some('é'));
    // A key that types nothing, and text that came from no one key.
    assert_eq!(unshifted("", "arrow_up", 0), Some('\0'));
    assert_eq!(unshifted("日本", "unidentified", 0), Some('\0'));
}

#[test]
fn notifications_coalesce_until_snapshot_acknowledges() {
    let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = count.clone();
    let wake = Wake {
        pending: AtomicBool::new(false),
        callback: Arc::new(move || {
            seen.fetch_add(1, Ordering::Relaxed);
        }),
    };
    for _ in 0..100 {
        wake.notify();
    }
    assert_eq!(count.load(Ordering::Relaxed), 1);
    wake.pending.store(false, Ordering::Release);
    wake.notify();
    assert_eq!(count.load(Ordering::Relaxed), 2);
}

fn client() -> (Client, mpsc::Receiver<Action>) {
    let (actions, queue) = mpsc::channel(256);
    let shared = Shared {
        state: Arc::default(),
        wake: Arc::new(Wake { pending: AtomicBool::new(false), callback: Arc::new(|| {}) }),
        herald: Herald::new().0,
    };
    let done = flume::bounded(0).1;
    let client = Client { actions, shared, done };
    (client, queue)
}

#[test]
fn input_is_accepted_only_while_connected() {
    let (client, mut queue) = client();
    assert!(client.try_send(Action::CreateSession(None)).is_err());
    client.try_send(Action::Reconnect).unwrap();
    client.shared.status("Connected", true);
    client.try_send(Action::CreateSession(None)).unwrap();
    assert!(matches!(queue.try_recv(), Ok(Action::Reconnect)));
    // Asking for something to be shown is numbered, to tell its answer by.
    let made = queue.try_recv();
    assert!(matches!(made, Ok(Action::CreateSession(None))));
    assert!(queue.try_recv().is_err());
}

#[test]
fn input_racing_a_disconnect_never_outlives_it() {
    let (client, mut queue) = client();
    let client = Arc::new(client);
    let stop = Arc::new(AtomicBool::new(false));
    let typing = {
        let (client, stop) = (client.clone(), stop.clone());
        std::thread::spawn(move || {
            while !stop.load(Ordering::Relaxed) {
                let _ = client.try_send(Action::CreateSession(None));
            }
        })
    };
    for _ in 0..20_000 {
        client.shared.status("Connected", true);
        std::thread::yield_now();
        // What `run` does when a connection fails.
        client.shared.status("Disconnected", false);
        while queue.try_recv().is_ok() {}
        // Input accepted before the failure is gone, and none may follow it.
        std::thread::yield_now();
        assert!(queue.try_recv().is_err(), "input outlived its connection");
    }
    stop.store(true, Ordering::Relaxed);
    typing.join().unwrap();
}

#[test]
fn what_is_asked_to_be_shown_is_shown_at_once_and_what_is_made_awaited() {
    let (client, mut queue) = client();
    let tab = |id: &str| Tab { id: id.into(), ..Tab::default() };
    let session = |id: &str, active: &str| Session {
        id: id.into(),
        name: id.into(),
        active: Some(active.into()),
        tabs: vec![tab("a"), tab("b")],
    };
    client.shared.change(|s| {
        s.view.connected = true;
        s.view.sessions = vec![session("s", "a"), session("t", "b")];
    });
    let shown = || {
        let view = client.snapshot();
        (view.selected_session, view.selected_tab)
    };
    let both = |session: &str, tab: &str| (Some(session.to_owned()), Some(tab.to_owned()));
    let show =
        |session: &str, tab: &str| Action::SelectTab { session: session.into(), tab: tab.into() };
    // A session comes with the tab the server shows of it.
    client.try_send(Action::SelectSession("s".into())).unwrap();
    assert_eq!(shown(), both("s", "a"));
    client.try_send(show("s", "b")).unwrap();
    assert_eq!(shown(), both("s", "b"));
    // A new tab is yet to be, and the one on show stays until it is.
    client.try_send(Action::CreateTab("s".into())).unwrap();
    assert_eq!(shown(), both("s", "b"));
    client.try_send(Action::CreateTab("t".into())).unwrap();
    assert_eq!(shown(), both("t", "b"));
    // What is not there is asked for all the same, and nothing is shown for it.
    client.try_send(show("gone", "a")).unwrap();
    assert_eq!(shown(), both("t", "b"));
    // What is to be made is awaited until something else is asked to be shown,
    // or the server is left. Input asks for nothing to be shown.
    let awaited = || client.shared.lock().awaited;
    assert!(!awaited());
    client.try_send(Action::CreateSession(None)).unwrap();
    let pane = "p".to_owned();
    client.try_send(Action::FocusPane { pane }).unwrap();
    assert!(awaited());
    client.try_send(show("s", "a")).unwrap();
    assert!(!awaited());
    client.try_send(Action::CreateTab("s".into())).unwrap();
    client.leave();
    assert!(!awaited());
    assert_eq!(shown(), (None, None));
    // All of it is asked of the server, in the order it came.
    assert_eq!(std::iter::from_fn(|| queue.try_recv().ok()).count(), 10);
}

#[test]
fn a_terminal_fed_no_more_is_kept_for_its_last_screen() {
    use crate::terminal::tests::{WAIT, encode, terminal, worker};
    let (client, _queue) = client();
    let mut screen = terminal(10, 2);
    screen.vt_write(b"last");
    let (drawn, wakes) = worker(encode(&mut screen));
    wakes.recv_timeout(WAIT).unwrap();
    // One has drawn its pane, and one has yet to.
    let (blank, _wakes) = worker(Vec::new());
    let pane = |id: &str| Pane { id: id.into(), ..Pane::default() };
    let tab = Tab { id: "w".into(), panes: vec![pane("drawn"), pane("blank")], ..Tab::default() };
    {
        let mut state = client.shared.lock();
        state.view.sessions =
            vec![Session { id: "s".into(), tabs: vec![tab], ..Session::default() }];
        state.view.selected_session = Some("s".into());
        state.terminals.insert("drawn".into(), drawn);
        state.terminals.insert("blank".into(), blank);
        // Their streams fail, or the connection does.
        state.rest("drawn");
        state.rest("blank");
    }
    // What was drawn is still shown, and can be asked for what is selected on it.
    // Nothing is typed into it: only a terminal that is fed takes input.
    let shown = |id: &str| client.snapshot().pane(id).is_some_and(|p| p.frame.is_some());
    assert!(shown("drawn") && client.terminal("drawn").is_ok());
    assert!(client.shared.terminal("drawn").is_none());
    assert!(!shown("blank") && client.terminal("blank").is_err());
}

/// A server of one connection at a socket of its own, which says what a test
/// has it say.
mod served {
    use super::*;
    use futures_util::SinkExt;
    use tokio::net::{UnixListener, UnixStream};
    use tokio_tungstenite::{
        WebSocketStream,
        tungstenite::{
            Message,
            handshake::server::{ErrorResponse, Request, Response},
        },
    };

    pub type Socket = WebSocketStream<UnixStream>;

    pub struct Server {
        path: std::path::PathBuf,
        listener: UnixListener,
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    impl Server {
        pub fn at(name: &str) -> Self {
            let name = format!("rexy-test-{name}-{}.sock", std::process::id());
            let path = std::env::temp_dir().join(name);
            let _ = std::fs::remove_file(&path);
            let listener = UnixListener::bind(&path).unwrap();
            Self { path, listener }
        }

        /// A client of it, and what the client's panes have to say.
        pub fn client(&self) -> (Client, flume::Receiver<crate::notify::Note>) {
            let endpoint = format!("unix://{}", self.path.display());
            let (herald, notes) = Herald::new();
            let theme = watch::channel(None).1;
            let client = Client::spawn(vec![endpoint], Arc::new(|| {}), herald, None, theme);
            (client, notes)
        }

        /// Take a client's connection and welcome it. None if the client has gone.
        pub async fn welcome(&self) -> Option<Socket> {
            let (stream, _) = self.listener.accept().await.unwrap();
            // The size of the error is the library's choice.
            #[allow(clippy::result_large_err)]
            let agree = |request: &Request, mut response: Response| {
                let protocol = request.headers()["Sec-WebSocket-Protocol"].clone();
                let headers = response.headers_mut();
                headers.insert("Sec-WebSocket-Protocol", protocol);
                Ok::<_, ErrorResponse>(response)
            };
            let socket = tokio_tungstenite::accept_hdr_async(stream, agree);
            let mut socket = socket.await.ok()?;
            socket.next().await?.ok()?;
            let welcome = json!({
                "type": "welcome",
                "protocol": "rex.control.v1",
                "api_revision": 13,
                "limits": {"max_message_bytes": 65536},
                "client": {"id": "client:1"},
            });
            let welcome = Message::Text(welcome.to_string().into());
            socket.send(welcome).await.ok()?;
            Some(socket)
        }
    }

    pub async fn say(socket: &mut Socket, message: Value) {
        let message = Message::Text(message.to_string().into());
        socket.send(message).await.unwrap();
    }

    /// The next thing asked: its number, its method and what it carries. None
    /// if the client has gone, or asks nothing for a while.
    pub async fn asked(socket: &mut Socket) -> Option<(String, String, Value)> {
        let next = async {
            loop {
                if let Message::Text(text) = socket.next().await?.ok()? {
                    return serde_json::from_str::<Value>(&text).ok();
                }
            }
        };
        let request = tokio::time::timeout(Duration::from_secs(2), next);
        let request = request.await.ok()??;
        let text = |key: &str| request[key].as_str().unwrap_or_default().to_owned();
        Some((text("id"), text("method"), request["payload"].clone()))
    }

    pub async fn answer(socket: &mut Socket, id: &str, payload: Value) {
        say(socket, json!({"type": "response", "id": id, "payload": payload})).await;
    }

    /// Answer what a client asks first, with the sessions there are, and what
    /// it asks next but for their views: those are handed back, by session.
    pub async fn listed(socket: &mut Socket, sessions: &[&str]) -> HashMap<String, String> {
        let (id, method, _) = asked(socket).await.unwrap();
        assert_eq!(method, "session.bootstrap");
        let named = |id: &&str| json!({"session_id": id, "label": id});
        let named: Vec<_> = sessions.iter().map(named).collect();
        let boot = json!({"attach_all": {"sessions": named}, "attached_sessions": []});
        answer(socket, &id, boot).await;
        // The view of each, and the host list.
        let mut views = HashMap::new();
        for _ in 0..=sessions.len() {
            let (id, method, payload) = asked(socket).await.unwrap();
            match method.as_str() {
                "session.view" => {
                    let session = payload["session_id"].as_str().unwrap();
                    views.insert(session.to_owned(), id);
                }
                _ => answer(socket, &id, json!({})).await,
            }
        }
        views
    }

    /// What a client asks next by `method`, for the test to answer. What it asks
    /// before that is answered with nothing, and noted in `seen`.
    pub async fn next(
        socket: &mut Socket,
        method: &str,
        seen: &mut Vec<(String, Value)>,
    ) -> (String, Value) {
        loop {
            let asked = asked(socket).await;
            let (id, asked, payload) = asked.unwrap_or_else(|| panic!("{method} was not asked"));
            if asked == method {
                return (id, payload);
            }
            // A terminal's stream is left waiting: one that failed would have
            // its pane take nothing typed.
            if asked != "data.connect" {
                answer(socket, &id, json!({})).await;
            }
            seen.push((asked, payload));
        }
    }

    /// A session's view at a revision: its tabs, each with the terminal that
    /// fills it and that terminal's owner, if it has either, and the tab the
    /// server shows.
    pub fn view(revision: u64, active: &str, tabs: &[(&str, Option<(&str, &str)>)]) -> Value {
        let tab = |(id, terminal): &(&str, Option<(&str, &str)>)| {
            let layer = terminal.map(|(block, owner)| {
                json!({
                    "kind": "tiled",
                    "layout": {"rect": [0, 0, 1, 1], "block_id": block},
                    "blocks": [{"block_id": block, "creator_name": TERMINAL, "owner": owner}],
                })
            });
            json!({"window_id": id, "layers": layer.into_iter().collect::<Vec<_>>()})
        };
        let tabs: Vec<_> = tabs.iter().map(tab).collect();
        json!({"revision": revision, "active_window_id": active, "windows": tabs})
    }

    /// News that a session's view has come to a revision.
    pub fn viewed(session: &str, revision: u64) -> Value {
        let event = json!({"type": "session_view_changed", "payload": {"revision": revision}});
        let within = json!({"session_id": session, "event": event});
        json!({"type": "event", "payload": {"type": "session_event", "payload": within}})
    }

    /// Wait for a client to be as a test would have it.
    pub async fn until(client: &Client, what: &str, ready: impl Fn(&View) -> bool) -> View {
        for _ in 0..200 {
            let view = client.snapshot();
            if ready(&view) {
                return view;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}");
    }
}

#[tokio::test]
async fn a_client_let_go_while_connecting_asks_nothing_of_the_server() {
    let server = served::Server::at("orphan");
    drop(server.client());
    // Only now does the server take the connection, or what is left of it, if
    // one was made at all.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let asked = async { served::asked(&mut server.welcome().await?).await };
    let asked = tokio::time::timeout(Duration::from_millis(500), asked).await;
    let asked = asked.ok().flatten().map(|(_, method, _)| method);
    assert_eq!(asked, None);
}

#[tokio::test]
async fn news_sent_ahead_of_an_answer_is_not_told_as_if_it_came_after() {
    use served::{answer, asked, say};
    let server = served::Server::at("news");
    let (client, notes) = server.client();
    let mut socket = server.welcome().await.unwrap();
    let views = served::listed(&mut socket, &["s1", "s2"]).await;
    let layer = json!({
        "kind": "tiled",
        "layout": {"rect": [0, 0, 1, 1], "block_id": "b1"},
        "blocks": [{"block_id": "b1", "creator_name": TERMINAL}],
    });
    let tab = json!({"window_id": "w1", "layers": [layer]});
    let view = json!({"revision": 1, "active_window_id": "w1", "windows": [tab]});
    answer(&mut socket, &views["s1"], view).await;
    // The terminal is asked about, three ways.
    let mut about = Vec::new();
    for _ in 0..3 {
        let (id, method, _) = asked(&mut socket).await.unwrap();
        about.push((method, id));
    }
    // It was waiting on the user, it went back to work, and then the answers
    // were written, which say it is at work.
    let status = |state: &str| {
        let record = json!({"id": "x", "state": state, "msg": "m"});
        let payload = json!({"id": "x", "record": record});
        let news = json!({"block_id": "b1", "name": "program_status_changed", "payload": payload});
        json!({"type": "event", "payload": {"type": "block_event", "payload": news}})
    };
    say(&mut socket, status("blocked")).await;
    say(&mut socket, status("working")).await;
    for (method, id) in &about {
        let payload = match method.as_str() {
            STATUS => json!({"records": [{"id": "x", "state": "working", "msg": "m"}]}),
            _ => json!({}),
        };
        answer(&mut socket, id, payload).await;
    }
    // The last view comes, and the client is connected.
    answer(&mut socket, &views["s2"], json!({"revision": 1, "windows": []})).await;
    let at_work = |view: &View| {
        let pane = view.find("b1").map(|(_, _, pane)| pane.activity);
        view.connected && pane == Some(Activity::Working)
    };
    served::until(&client, "a pane at work", at_work).await;
    let said: Vec<String> = notes.try_iter().map(|note| note.body).collect();
    assert!(said.is_empty(), "old news was told: {said:?}");
}

#[tokio::test]
async fn a_view_that_does_not_come_is_said_and_asked_for_again() {
    let server = served::Server::at("view");
    let (client, _notes) = server.client();
    let mut socket = server.welcome().await.unwrap();
    let views = served::listed(&mut socket, &["s1"]).await;
    let error = json!({"code": "internal", "message": "busy"});
    let refusal = json!({"type": "response", "id": views["s1"], "error": error});
    served::say(&mut socket, refusal).await;
    // In the server's words, without its code.
    let said = |view: &View| view.error.is_some();
    let view = served::until(&client, "its saying so", said).await;
    assert_eq!(view.error.as_deref(), Some("A session's tabs were not read: busy"));
    let view = served::until(&client, "a connection", |view| view.connected).await;
    assert_eq!(view.sessions.len(), 1);
    // Asked for again before long, its tabs come.
    let began = std::time::Instant::now();
    let again = loop {
        match served::asked(&mut socket).await {
            Some((id, method, _)) if method == "session.view" => break id,
            _ => assert!(began.elapsed() < RETRY[0] * 3, "it was not asked for again"),
        }
    };
    let tab = json!({"window_id": "w1", "layers": []});
    let view = json!({"revision": 1, "active_window_id": "w1", "windows": [tab]});
    served::answer(&mut socket, &again, view).await;
    let tabs = |view: &View| view.sessions[0].tabs.len() == 1;
    served::until(&client, "its tabs", tabs).await;
}

#[tokio::test]
async fn what_is_asked_of_a_client_still_connecting_waits_for_it_to_connect() {
    let server = served::Server::at("early");
    let (client, _notes) = server.client();
    let mut socket = server.welcome().await.unwrap();
    let views = served::listed(&mut socket, &["s1"]).await;
    let asked = json!({"call_id": "c1", "action": "pane.zoom", "args": {}});
    let asked = json!({"type": "client_perform_requested", "payload": asked});
    served::say(&mut socket, json!({"type": "event", "payload": asked})).await;
    // Neither refused nor passed to a window with nothing yet to do it to.
    let moment = Duration::from_millis(200);
    let refused = tokio::time::timeout(moment, served::asked(&mut socket)).await;
    assert!(refused.is_err(), "it was answered: {refused:?}");
    assert!(client.snapshot().asked.is_empty());
    let view = json!({"revision": 1, "windows": []});
    served::answer(&mut socket, &views["s1"], view).await;
    let passed = |view: &View| !view.asked.is_empty();
    let view = served::until(&client, "its being passed on", passed).await;
    assert!(view.connected);
    assert_eq!(view.asked[0].call.as_deref(), Some("c1"));
}

#[tokio::test]
async fn a_server_that_closes_while_a_client_begins_is_said_to_have_and_why() {
    use tokio_tungstenite::tungstenite::protocol::{CloseFrame, frame::coding::CloseCode};
    let server = served::Server::at("closed");
    let (client, _notes) = server.client();
    let mut socket = server.welcome().await.unwrap();
    let (_, method, _) = served::asked(&mut socket).await.unwrap();
    assert_eq!(method, "session.bootstrap");
    let frame = CloseFrame { code: CloseCode::from(1008), reason: "event backlog exceeded".into() };
    socket.close(Some(frame)).await.unwrap();
    // It is said that it will be tried again, and why.
    let lost = |view: &View| view.status.starts_with("Retrying · ");
    let view = served::until(&client, "its end", lost).await;
    assert!(view.status.contains("fell too far behind"), "{}", view.status);
}

#[tokio::test]
async fn a_tab_picked_stays_on_show_and_one_made_here_is_shown_when_it_comes() {
    use served::{answer, next, say, until, view, viewed};
    let server = served::Server::at("tabs");
    let (client, _notes) = server.client();
    let mut socket = server.welcome().await.unwrap();
    let views = served::listed(&mut socket, &["s1"]).await;
    let tabs = [("w1", None), ("w2", None)];
    answer(&mut socket, &views["s1"], view(1, "w1", &tabs)).await;
    until(&client, "a connection", |view| view.connected).await;
    let seen = &mut Vec::new();
    // Its session is joined when it is asked to be shown, with the server's tab.
    client.try_send(Action::SelectSession("s1".into())).unwrap();
    let (id, joined) = next(&mut socket, "session.attach", seen).await;
    assert_eq!(joined["session_id"], "s1");
    answer(&mut socket, &id, json!({"view": view(1, "w1", &tabs)})).await;
    let shown = |tab: &'static str| move |view: &View| view.selected_tab.as_deref() == Some(tab);
    until(&client, "the server's tab", shown("w1")).await;
    // Another tab is shown as soon as it is picked, and the server is told.
    let (session, tab) = ("s1".to_owned(), "w2".to_owned());
    client.try_send(Action::SelectTab { session, tab }).unwrap();
    assert!(shown("w2")(&client.snapshot()));
    let (id, picked) = next(&mut socket, "session.focus_window", seen).await;
    assert_eq!((&picked["session_id"], &picked["window_id"]), (&json!("s1"), &json!("w2")));
    answer(&mut socket, &id, json!({"revision": 2})).await;
    let (id, _) = next(&mut socket, "session.view", seen).await;
    answer(&mut socket, &id, view(2, "w2", &tabs)).await;
    until(&client, "the tab picked", shown("w2")).await;
    // Another client has the server show the first again, and makes a tab
    // besides. Neither moves this window from the tab it shows.
    say(&mut socket, viewed("s1", 3)).await;
    let (id, _) = next(&mut socket, "session.view", seen).await;
    let more = [("w1", None), ("w2", None), ("w3", None)];
    answer(&mut socket, &id, view(3, "w1", &more)).await;
    let listed = |view: &View| view.sessions[0].tabs.len() == 3;
    assert!(shown("w2")(&until(&client, "the tab made elsewhere", listed).await));
    // A tab made here is shown once the session has it.
    client.try_send(Action::CreateTab("s1".into())).unwrap();
    let (id, _) = next(&mut socket, "session.new_window", seen).await;
    answer(&mut socket, &id, json!({"window_id": "w4", "revision": 4})).await;
    let (id, _) = next(&mut socket, "session.view", seen).await;
    assert!(shown("w2")(&client.snapshot()));
    let more = [("w1", None), ("w2", None), ("w3", None), ("w4", None)];
    answer(&mut socket, &id, view(4, "w4", &more)).await;
    until(&client, "the tab made here", shown("w4")).await;
    // When the tab on show closes, the server's is shown.
    say(&mut socket, viewed("s1", 5)).await;
    let (id, _) = next(&mut socket, "session.view", seen).await;
    answer(&mut socket, &id, view(5, "w1", &tabs)).await;
    until(&client, "the server's tab", shown("w1")).await;
}

#[tokio::test]
async fn the_panes_of_a_tab_are_fed_and_taken_as_it_comes_on_show_and_given_up_as_it_is_left() {
    use served::{answer, next, say, until, view, viewed};
    let server = served::Server::at("panes");
    let (client, _notes) = server.client();
    let mut socket = server.welcome().await.unwrap();
    let views = served::listed(&mut socket, &["s1"]).await;
    // Its one terminal is another client's.
    let theirs = [("w1", Some(("b1", "client:2")))];
    answer(&mut socket, &views["s1"], view(1, "w1", &theirs)).await;
    until(&client, "a connection", |view| view.connected).await;
    let seen = &mut Vec::new();
    client.try_send(Action::SelectSession("s1".into())).unwrap();
    let (id, _) = next(&mut socket, "session.attach", seen).await;
    answer(&mut socket, &id, json!({"view": view(1, "w1", &theirs)})).await;
    // The terminal is taken, to be the size this window gives it, and is fed.
    let (id, taken) = next(&mut socket, "session.own_block", seen).await;
    assert_eq!(taken, json!({"session_id": "s1", "block_ids": ["b1"], "mode": "take"}));
    answer(&mut socket, &id, json!({})).await;
    let (id, fed) = next(&mut socket, "data.connect", seen).await;
    assert_eq!((&fed["session_id"], &fed["block_id"]), (&json!("s1"), &json!("b1")));
    answer(&mut socket, &id, json!({})).await;
    // It is this client's, and then the other takes it back, having the tab
    // come on show there. It is not taken again for that: the two would take
    // it from each other without end.
    seen.clear();
    let owned = |view: &View| view.pane("b1").is_some_and(|pane| pane.owner);
    let mine = [("w1", Some(("b1", "client:1")))];
    for (revision, tabs, ours) in [(2, &mine, true), (3, &theirs, false)] {
        say(&mut socket, viewed("s1", revision)).await;
        let (id, _) = next(&mut socket, "session.view", seen).await;
        answer(&mut socket, &id, view(revision, "w1", tabs)).await;
        until(&client, "its owner known", |view| owned(view) == ours).await;
    }
    // Left, its panes are given up for a client that wants them at its own size.
    client.leave();
    let (_, left) = next(&mut socket, "session.disown_block", seen).await;
    assert_eq!(left, json!({"session_id": "s1"}));
    let again = |(method, _): &(String, Value)| method == "session.own_block";
    assert!(!seen.iter().any(again), "{seen:?}");
}

#[tokio::test]
async fn what_the_user_has_the_server_do_moves_the_window_with_the_servers_tab() {
    use served::{answer, next, say, until, view, viewed};
    let server = served::Server::at("moved");
    let (client, _notes) = server.client();
    let mut socket = server.welcome().await.unwrap();
    let views = served::listed(&mut socket, &["s1"]).await;
    let tabs = |active: &str, revision| {
        view(revision, active, &[("w1", Some(("b1", "client:1"))), ("w2", None)])
    };
    answer(&mut socket, &views["s1"], tabs("w1", 1)).await;
    until(&client, "a connection", |view| view.connected).await;
    let seen = &mut Vec::new();
    client.try_send(Action::SelectSession("s1".into())).unwrap();
    let (id, _) = next(&mut socket, "session.attach", seen).await;
    answer(&mut socket, &id, json!({"view": tabs("w1", 1)})).await;
    let shown = |tab: &'static str| move |view: &View| view.selected_tab.as_deref() == Some(tab);
    until(&client, "its tab", shown("w1")).await;
    // Another client has the server show the second tab, which moves nothing here.
    say(&mut socket, viewed("s1", 2)).await;
    let (id, _) = next(&mut socket, "session.view", seen).await;
    answer(&mut socket, &id, tabs("w2", 2)).await;
    let elsewhere = |view: &View| view.sessions[0].active.as_deref() == Some("w2");
    assert!(shown("w1")(&until(&client, "the server's tab known", elsewhere).await));
    // A key that a function of the server's answers. The server is first told
    // the tab the user is looking at, for the function to act on that one.
    let key = |served| Action::Key {
        pane: "b1".into(),
        key: "t".into(),
        code: "key_t".into(),
        modifiers: 0,
        pressed: true,
        repeat: false,
        served,
    };
    client.try_send(key(Some(Vec::new()))).unwrap();
    let (_, told) = next(&mut socket, "session.focus_window", seen).await;
    assert_eq!(told, json!({"session_id": "s1", "window_id": "w1"}));
    let (id, _) = next(&mut socket, "client.key", seen).await;
    // The function shows the second tab, and the window goes there with it.
    answer(&mut socket, &id, json!({"handled": true})).await;
    let (id, _) = next(&mut socket, "session.view", seen).await;
    answer(&mut socket, &id, tabs("w2", 3)).await;
    until(&client, "the tab the function showed", shown("w2")).await;
    // A key that no function took after all asks nothing more, nor does one
    // that none was bound to.
    for served in [Some(Vec::new()), None] {
        client.try_send(key(served)).unwrap();
        let (id, _) = next(&mut socket, "client.key", seen).await;
        answer(&mut socket, &id, json!({"handled": false})).await;
    }
    let more = tokio::time::timeout(Duration::from_millis(200), served::asked(&mut socket));
    assert!(more.await.is_err());
    assert!(shown("w2")(&client.snapshot()));
}
