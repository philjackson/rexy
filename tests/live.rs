//! Run only against a disposable server, never the default personal socket.
use rexy::{
    backend::Client,
    model::{self, Action, Activity, Edit, Host, Size, Tab, View, Wait},
    notify::Herald,
    terminal::Frame,
    upload::File,
};
use serde_json::{Value, json};
use std::{
    sync::Arc,
    time::{Duration, Instant},
};

fn wait(client: &Client, label: &str, ready: impl Fn(&View) -> bool) -> View {
    let start = Instant::now();
    loop {
        let view = client.snapshot();
        if ready(&view) {
            return view;
        }
        assert!(
            start.elapsed() < Duration::from_secs(15),
            "{label}: {} ({:?})",
            view.status,
            view.panes().map(|p| &p.status).collect::<Vec<_>>()
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn frame<'a>(view: &'a View, pane: &str) -> Option<&'a Arc<Frame>> {
    view.pane(pane)?.frame.as_ref()
}

fn screen(view: &View, pane: &str) -> String {
    frame(view, pane).map(|frame| frame.text()).unwrap_or_default()
}

fn find<'a>(view: &'a View, id: &str) -> Option<&'a Tab> {
    view.tabs().iter().find(|tab| tab.id == id)
}

fn send(client: &Client, action: Action) {
    client.try_send(action).unwrap();
}

/// A key going down in a pane or coming up, alone.
fn key(pane: &str, key: &str, code: &str, modifiers: u8, pressed: bool) -> Action {
    Action::Key {
        pane: pane.into(),
        key: key.into(),
        code: code.into(),
        modifiers,
        pressed,
        repeat: false,
        served: None,
    }
}

fn enter(client: &Client, pane: &str) {
    for pressed in [true, false] {
        send(client, key(pane, "\n", "enter", 0, pressed));
    }
}

/// A client of the server, with the keys a window has.
fn open(endpoint: &str, herald: Herald) -> Client {
    let keymap = Some(rexy::ui::commands::request());
    let theme = tokio::sync::watch::channel(None).1;
    Client::spawn(vec![endpoint.to_owned()], Arc::new(|| {}), herald, keymap, theme)
}

/// A terminal has been drawn.
fn drawn(view: &View) -> bool {
    view.panes().any(|pane| pane.frame.is_some())
}

/// The server to test against, which must be one made for it.
fn endpoint() -> String {
    let endpoint = std::env::var("REXY_TEST_SERVER").expect("explicit test endpoint required");
    assert!(
        endpoint.starts_with("unix://") && endpoint.contains("rexy") && endpoint.contains("/work/"),
        "refusing to mutate a non-test Rex server"
    );
    endpoint
}

/// Run the `rex` command against that server and no other: what it printed, or
/// what it said had gone wrong.
fn rex(endpoint: &str, args: &[&str]) -> Result<String, String> {
    let mut rex = std::process::Command::new("rex");
    let rex = rex.args(["-S", endpoint, "--autostart=false"]).args(args);
    let rex = rex.output().unwrap();
    match rex.status.success() {
        true => Ok(String::from_utf8(rex.stdout).unwrap()),
        false => Err(String::from_utf8_lossy(&rex.stderr).into_owned()),
    }
}

/// A session made for a test, with a shell in its one pane, and a client that
/// shows it.
fn shell(endpoint: &str, name: &str) -> (String, String, Client) {
    let shell = ["new", name, "--shell=none", "--keep-open", "--json", "--", "/bin/sh"];
    let made: Value = serde_json::from_str(&rex(endpoint, &shell).unwrap()).unwrap();
    let session = made["session_id"].as_str().unwrap().to_owned();
    let pane = made["initial_windows"][0]["block_ids"][0].as_str().unwrap().to_owned();
    let client = open(endpoint, Herald::new().0);
    wait(&client, "a connection", |v| v.connected);
    send(&client, Action::SelectSession(session.clone()));
    wait(&client, "the pane", |v| frame(v, &pane).is_some());
    (session, pane, client)
}

/// Ask that server something as the command line does.
fn call(endpoint: &str, method: &str, payload: Value) -> Result<Value, String> {
    let reply = rex(endpoint, &["api", "call", method, &payload.to_string()])?;
    Ok(serde_json::from_str(&reply).unwrap())
}

#[test]
#[ignore = "requires REXY_TEST_SERVER pointing to a disposable Rex server"]
fn live_input_resize_splits_tabs_sessions_and_reconnect() {
    let endpoint = endpoint();
    let (herald, notes) = Herald::new();
    let client = open(&endpoint, herald);
    // A connection comes with every session's tabs, and with no session shown
    // unasked. A window asks for the first, of the server it shows.
    let listed = wait(&client, "a connection", |v| v.connected);
    assert!(listed.sessions.iter().all(|s| !s.tabs.is_empty()));
    assert!(listed.selected_session.is_none() && listed.tabs().is_empty());
    send(&client, listed.unshown().unwrap());
    let initial = wait(&client, "initial Ghostty snapshot", drawn);
    let session = initial.selected_session.clone().unwrap();
    let tab = initial.selected_tab.clone().unwrap();
    let pane = initial.current_tab().unwrap().panes[0].id.clone();
    // The pane was made by a client that has left the server. Shown here, it
    // becomes this client's without being touched.
    let owned = |v: &View| v.pane(&pane).is_some_and(|p| p.owner);
    wait(&client, "a pane whose owner has gone", owned);
    send(&client, Action::FocusPane { pane: pane.clone() });
    let size = Size { cols: 100, rows: 32, cell_width: 9, cell_height: 19 };
    let resize = Action::Resize { pane: pane.clone(), size };
    send(&client, resize);
    wait(&client, "server resize metadata", |v| {
        frame(v, &pane).is_some_and(|f| f.cols == 100 && f.rows == 32)
    });

    // Type a command and run it.
    let run = |command: &str| {
        let (id, text) = (pane.clone(), command.into());
        send(&client, Action::Text { pane: id, text });
        enter(&client, &pane);
    };
    // Split the marker in the command so seeing the typed command cannot satisfy the assertion.
    run("printf 'rexy-%s\\n' verified");
    wait(&client, "keyboard input and live output", |v| screen(v, &pane).contains("rexy-verified"));

    let paste = Action::Paste { pane: pane.clone(), text: "printf 'paste-%s\\n' verified".into() };
    send(&client, paste);
    enter(&client, &pane);
    wait(&client, "paste and live output", |v| screen(v, &pane).contains("paste-verified"));

    // A program that asks is told where the mouse moves and of each step of the
    // wheel, and can tell Shift+Tab and Shift+Enter from Tab and Enter. It reads
    // one line, which the terminal shows as it comes.
    let tracking = |v: &View| frame(v, &pane).is_some_and(|f| f.mouse_tracking);
    run("printf '\\033[?1003h\\033[?1006h\\033[>1u'; head -n 1 | cat -v; \
        printf '\\033[<u\\033[?1003l\\033[?1006l'");
    wait(&client, "a program tracking the mouse", tracking);
    let (id, x, y) = (pane.clone(), 3, 4);
    let motion =
        Action::Mouse { pane: pane.clone(), kind: "motion", x, y, button: None, modifiers: 0 };
    send(&client, motion);
    for _ in 0..3 {
        let (pane, lines) = (id.clone(), -1);
        send(&client, Action::Wheel { pane, x, y, lines });
    }
    for (text, code) in [("\t", "tab"), ("\n", "enter")] {
        send(&client, key(&pane, text, code, model::SHIFT, true));
    }
    wait(&client, "the mouse and shifted keys reported", |v| {
        let screen = screen(v, &pane);
        let wheel = screen.matches("[<64;4;5M").count();
        let keys = screen.contains("[9;2u") && screen.contains("[13;2u");
        screen.contains("[<35;4;5M") && wheel == 3 && keys
    });
    enter(&client, &pane);
    wait(&client, "the mouse let go", |v| !tracking(v));

    let old_frame = frame(&client.snapshot(), &pane).unwrap().clone();
    send(&client, Action::Reconnect);
    // The server knows the client again, with what it owned: nothing else here
    // would give the pane back to it untouched while the server still held its place.
    wait(&client, "fresh reconnect snapshot", |v| {
        v.connected
            && owned(v)
            && frame(v, &pane).is_some_and(|f| !Arc::ptr_eq(f, &old_frame))
            && screen(v, &pane).contains("rexy-verified")
    });
    // A shell is known to be where it is without saying so itself, and a split
    // starts where the pane it is made from is.
    let at_tmp = |v: &View, pane: &str| v.pane(pane).is_some_and(|p| p.title.ends_with(" /tmp"));
    run("cd /tmp");
    wait(&client, "a shell's directory", |v| at_tmp(v, &pane));
    let split = Action::Split { pane: pane.clone(), toward: [1.0, 0.0] };
    send(&client, split);
    let split = wait(&client, "split layout and streams", |v| {
        find(v, &tab)
            .is_some_and(|w| w.panes.len() == 2 && w.panes.iter().all(|p| p.frame.is_some()))
    });
    let mut others = find(&split, &tab).unwrap().panes.iter();
    let other = others.find(|p| p.id != pane).unwrap().id.clone();
    wait(&client, "a split starting where its pane is", |v| at_tmp(v, &other));
    // Two panes change places, and change back.
    let first = |v: &View| find(v, &tab).and_then(|w| Some(w.borders.first()?.before.clone()));
    for then in [&other, &pane] {
        send(&client, Action::Swap { pane: pane.clone(), with: other.clone() });
        wait(&client, "two panes changing places", |v| first(v).as_ref() == Some(then));
    }
    // The border between the two moves where it is asked to, and the server gives
    // each side its share. Asked faster than the server answers, as a drag asks,
    // it comes to rest where it was last asked to go.
    let [border] = &find(&split, &tab).unwrap().borders[..] else {
        panic!("a split of two has one border");
    };
    let sides = (&border.before, &border.after, border.upright);
    assert_eq!(sides, (&pane, &other, true));
    let divide = |ratio: f32| Action::Divide(vec![border.moved(ratio)]);
    let divided = |ratio: f32| {
        wait(&client, "a border moved", |v| {
            let near = |at: f32| (at - ratio).abs() < 0.01;
            find(v, &tab).is_some_and(|w| near(w.borders[0].at) && near(w.panes[0].width))
        });
    };
    send(&client, divide(0.3));
    divided(0.3);
    for step in 0..=20 {
        send(&client, divide(0.5 + 0.01 * step as f32));
    }
    divided(0.7);
    send(&client, divide(0.5));
    divided(0.5);
    // The server is told some things from outside: this window types only into
    // what it shows.
    let rex = |args: &[&str]| rex(&endpoint, args).unwrap();
    // A pane fills its tab in place of the rest, and gives it back to them.
    for (label, zoomed, panes) in [("a pane zoomed", true, 1), ("a pane restored", false, 2)] {
        send(&client, Action::Zoom { pane: pane.clone() });
        wait(&client, label, |v| {
            // One pane alone has no border to drag.
            let shown = |w: &Tab| {
                w.panes.len() == panes && w.panes[0].id == pane && w.borders.len() == panes - 1
            };
            find(v, &tab).is_some_and(|w| w.zoomed == zoomed && shown(w))
        });
        if !zoomed {
            continue;
        }
        // The pane behind it runs on, and what it has to say is still heard.
        let report = "printf '\\033]7501;state=done\\007'\n";
        rex(&["send", "-s", &session, "-b", &other, report]);
        wait(&client, "a pane out of sight heard", |v| {
            let behind = find(v, &tab).and_then(|w| w.hidden.first());
            behind.is_some_and(|p| p.id == other && p.activity == Activity::Done)
        });
        let note = notes.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!((note.pane.as_str(), note.body.as_str()), (other.as_str(), "Done"));
    }
    let call = |method: &str, payload: Value| call(&endpoint, method, payload).unwrap();
    // A pane is made to float over the tab's layout, with the keyboard. The
    // layout keeps its border under it.
    let holds = |v: &View, shown: usize, hidden: &[&str]| {
        let hides = |w: &Tab| w.hidden.iter().map(|p| p.id.as_str()).eq(hidden.iter().copied());
        find(v, &tab).is_some_and(|w| w.panes.len() == shown && hides(w))
    };
    let floats = |v: &View| find(v, &tab).map_or(Vec::new(), |w| w.floats.clone());
    let borders = |v: &View| find(v, &tab).unwrap().borders.len();
    let over = [0.2, 0.2, 0.5, 0.5];
    send(&client, Action::NewFloat { session: session.clone(), tab: tab.clone(), bounds: over });
    let made = wait(&client, "a floating pane made", |v| {
        v.made.is_some() && holds(v, 3, &[]) && floats(v).len() == 1
    });
    let (floating, layer) = (made.made.clone().unwrap(), floats(&made)[0].id.clone());
    let floated = |v: &View| find(v, &tab).and_then(|w| w.panes.last()).map(|p| p.float.clone());
    assert_eq!((floated(&made), borders(&made)), (Some(Some(layer.clone())), 1));
    assert_eq!(find(&made, &tab).unwrap().focus.as_ref(), Some(&floating));
    // It is moved, and given another size, where it floats.
    let placed = [0.25, 0.125, 0.5, 0.75];
    let place = Action::Place { session: session.clone(), layer: layer.clone(), bounds: placed };
    send(&client, place);
    wait(&client, "a floating pane moved", |v| {
        floats(v).first().is_some_and(|f| f.bounds == placed)
    });
    // Put away, it runs on out of sight, and comes back with the keyboard:
    // brought back with its layer, or revealed alone.
    let stow = |back| Action::Stow { session: session.clone(), layers: vec![layer.clone()], back };
    for back in [stow(Some(floating.clone())), Action::Reveal { pane: floating.clone() }] {
        send(&client, stow(None));
        wait(&client, "a pane put away", |v| holds(v, 2, &[&floating]) && floats(v)[0].away);
        send(&client, back);
        wait(&client, "a pane brought back", |v| {
            v.made.as_ref() == Some(&floating) && holds(v, 3, &[]) && !floats(v)[0].away
        });
    }
    // Another floats in front of it, until the first is given the keyboard.
    let beyond = [0.5, 0.5, 0.25, 0.25];
    send(&client, Action::NewFloat { session: session.clone(), tab: tab.clone(), bounds: beyond });
    let two = wait(&client, "a second floating pane", |v| v.made.is_some() && floats(v).len() == 2);
    assert_eq!(floats(&two)[0].id, layer);
    send(&client, Action::FocusPane { pane: floating.clone() });
    wait(&client, "a floating pane to the front", |v| {
        floats(v).last().is_some_and(|f| f.id == layer)
    });
    send(&client, Action::ClosePane { pane: two.made.clone().unwrap() });
    wait(&client, "the second closed", |v| holds(v, 3, &[]) && floats(v).len() == 1);
    // It goes back into the layout beside a pane there, and out of it again.
    send(&client, Action::Dock { pane: floating.clone(), beside: pane.clone() });
    let docked = wait(&client, "a pane docked", |v| holds(v, 3, &[]) && floats(v).is_empty());
    assert_eq!(borders(&docked), 2);
    send(&client, Action::Float { pane: floating.clone(), bounds: over });
    let again =
        wait(&client, "a pane floated", |v| floats(v).first().is_some_and(|f| f.bounds == over));
    assert_eq!(borders(&again), 1);
    send(&client, Action::ClosePane { pane: floating });
    let closed = wait(&client, "a floating pane closed", |v| holds(v, 2, &[]));
    assert_eq!((borders(&closed), floats(&closed).len()), (1, 0));
    send(&client, Action::CreateTab(session.clone()));
    let next = wait(&client, "new tab", |v| {
        v.tabs().len() == initial.tabs().len() + 1
            && v.selected_tab.as_ref() != Some(&tab)
            && v.current_tab().is_some_and(|w| w.panes.iter().all(|p| p.frame.is_some()))
    });
    assert_eq!(next.selected_session.as_ref(), Some(&session));
    let show = |tab: &str| Action::SelectTab { session: session.clone(), tab: tab.into() };
    send(&client, show(&tab));
    wait(&client, "return to existing tab", |v| {
        v.selected_tab.as_ref() == Some(&tab) && screen(v, &pane).contains("paste-verified")
    });

    // A tab shown before is kept fed while it is out of sight. Going back to
    // one finds it drawn, with nothing to wait for.
    let added = next.selected_tab.clone().unwrap();
    send(&client, show(&added));
    assert!(client.snapshot().panes().all(|pane| pane.frame.is_some()));
    send(&client, show(&tab));
    assert!(screen(&client.snapshot(), &pane).contains("paste-verified"));

    // A tab goes by its name if it has one, and otherwise by its terminal's title.
    let edit = |session: &str, tab: Option<&str>, edit| Action::Edit {
        session: session.into(),
        tab: tab.map(str::to_owned),
        edit,
    };
    let rename = |name: &str| edit(&session, Some(&tab), Edit::Rename(name.into()));
    send(&client, rename("Named"));
    wait(&client, "a tab renamed", |v| find(v, &tab).is_some_and(|w| w.name() == "Named"));
    send(&client, rename(""));
    // Of its panes, the one that typing goes to. A program that gives itself a
    // title goes by it, and a shell back at its prompt by its name and where
    // it is, which the server says without the shell's help.
    send(&client, Action::FocusPane { pane: pane.clone() });
    run("printf '\\033]2;%s\\007' Titled; sleep 2");
    wait(&client, "a tab going by its terminal's title", |v| {
        find(v, &tab).is_some_and(|w| w.name() == "Titled")
    });
    wait(&client, "a tab going by its shell and where that is", |v| {
        at_tmp(v, &pane) && find(v, &tab).is_some_and(|w| w.name().ends_with(" /tmp"))
    });
    // Programs say what they are doing, and what they wait for or end with is
    // passed on, for whoever is not looking. The message is "Run it?" in base64.
    let of = format!(" — {}", client.snapshot().session().unwrap().name);
    let said = |report: &str, activity, note: Option<&str>| {
        run(&format!("printf '\\033]7501;{report}\\007'"));
        wait(&client, "a program's status", |v| {
            v.pane(&pane).is_some_and(|p| p.activity == activity)
        });
        let heard = notes.recv_timeout(Duration::from_millis(300)).ok();
        assert_eq!(heard.as_ref().map(|note| note.body.as_str()), note);
        // It says which tab, and of which session.
        let about = heard.map(|note| (note.tab == tab, note.summary));
        let placed = |(tab, summary): &(bool, String)| *tab && summary.ends_with(&of);
        assert!(about.as_ref().is_none_or(placed), "{about:?}");
    };
    let asked = Activity::Blocked(Wait::Approval);
    let ask = "state=blocked:kind=permission:msg=UnVuIGl0Pw";
    said(ask, asked, Some("Needs approval: Run it?"));
    assert_eq!(client.snapshot().pane(&pane).unwrap().message, "Run it?");
    said("state=working", Activity::Working, None);
    // What a program comes to while the connection is lost is news when it is back.
    run("sleep 1; printf '\\033]7501;state=%s\\007' error");
    wait(&client, "a report to come", |v| screen(v, &pane).contains("sleep 1"));
    let old_frame = frame(&client.snapshot(), &pane).unwrap().clone();
    send(&client, Action::Reconnect);
    wait(&client, "a status that changed out of hearing", |v| {
        let fresh = frame(v, &pane).is_some_and(|f| !Arc::ptr_eq(f, &old_frame));
        v.connected && fresh && v.pane(&pane).is_some_and(|p| p.activity == Activity::Error)
    });
    let late = notes.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(late.body, "Error");
    said("state=done", Activity::Done, Some("Done"));
    // Running something else does not take back what a program ended with.
    run("true");
    said("state=error", Activity::Error, Some("Error"));
    said("state=idle", Activity::Idle, None);
    // A bell is news to the window as well, and a notice is for the desktop alone.
    run("printf '\\007'");
    wait(&client, "a bell", |v| v.rang.contains(&pane));
    let bell = notes.recv_timeout(Duration::from_secs(1)).unwrap();
    assert_eq!(bell.body, "Bell");
    run("printf '\\033]777;notify;Build;All green\\007'");
    let notice = notes.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(notice.body, "Build: All green");
    // A tab has the icon of the program in its foreground, or of the one that
    // reports from it.
    let program = |label: &str, icon: &str| {
        wait(&client, label, |v| find(v, &tab).is_some_and(|w| w.program().name() == icon));
    };
    program("a shell", "shell");
    run("sleep 1");
    program("a program in the foreground", "shell-sleep");
    program("the shell again", "shell");
    run("printf '\\033]7501;state=working:app=claude-code\\007'");
    program("an agent, by its report", "claude-code");
    run("printf '\\033]7501;state=clear\\007'");
    program("the shell once more", "shell");
    // What a program copies reaches the client, short or long.
    let copy = |text: &str| format!("printf '\\033]52;c;%s\\007' \"$({text} | base64 -w0)\"");
    run(&copy("printf copied-text"));
    wait(&client, "a program's copy", |v| {
        v.copied.as_ref().is_some_and(|c| c.0 == pane && c.1 == "copied-text")
    });
    run(&copy("head -c 1500000 /dev/zero | tr '\\0' x"));
    wait(&client, "a long copy", |v| {
        let long = |text: &str| text.len() == 1_500_000 && text.bytes().all(|b| b == b'x');
        v.copied.as_ref().is_some_and(|c| c.0 == pane && long(&c.1))
    });
    // A pane closed leaves the tab to the others.
    send(&client, Action::ClosePane { pane: other });
    wait(&client, "a pane closed", |v| {
        find(v, &tab).is_some_and(|w| !w.split() && w.panes[0].id == pane)
    });
    let order = |v: &View| -> Vec<String> { v.tabs().iter().map(|w| w.id.clone()).collect() };
    let first = Edit::MoveBefore(Some(tab.clone()));
    send(&client, edit(&session, Some(&added), first));
    wait(&client, "a tab moved first", |v| order(v).first() == Some(&added));
    send(&client, edit(&session, Some(&added), Edit::MoveBefore(None)));
    wait(&client, "a tab moved last", |v| order(v).last() == Some(&added));

    // A session made with a name is shown at once, and ending it shows another.
    send(&client, Action::CreateSession(Some("Second".into())));
    let second = wait(&client, "a named session", |v| {
        let shown = v.session().is_some_and(|s| s.name == "Second");
        shown && drawn(v)
    });
    let second = second.selected_session.unwrap();
    // The tabs of the session left behind are still listed.
    let behind = client.snapshot();
    let behind = behind.sessions.iter().find(|s| s.id == session).unwrap();
    assert_eq!(behind.tabs.len(), initial.tabs().len() + 1);
    send(&client, edit(&second, None, Edit::Rename("Renamed".into())));
    wait(&client, "a session renamed", |v| v.session().is_some_and(|s| s.name == "Renamed"));
    let first = Edit::MoveBefore(Some(session.clone()));
    send(&client, edit(&second, None, first));
    wait(&client, "a session moved first", |v| v.sessions[0].id == second);
    // A window that opens is shown the server's first session, and asks at once
    // to be where it last was instead. That is where it comes to be.
    let again = open(&endpoint, Herald::new().0);
    let opened = wait(&again, "a window opened again", |v| v.connected);
    send(&again, opened.unshown().unwrap());
    send(&again, show(&tab));
    wait(&again, "a window back where it was", |v| {
        v.selected_tab.as_ref() == Some(&tab) && drawn(v)
    });
    drop(again);
    // Its last tab closed, a session remains, empty.
    let only = client.snapshot().tabs()[0].id.clone();
    send(&client, edit(&second, Some(&only), Edit::Close));
    wait(&client, "a session without tabs", |v| {
        v.selected_session.as_ref() == Some(&second) && v.tabs().is_empty()
    });
    send(&client, edit(&second, None, Edit::Close));
    let ended = wait(&client, "a session ended", |v| {
        v.sessions.iter().all(|s| s.id != second) && v.selected_session.is_none()
    });
    send(&client, ended.unshown().unwrap());
    wait(&client, "the first session shown in its place", |v| {
        v.selected_session.as_ref() == Some(&session) && drawn(v)
    });
    // A tab of a session shown again is shown with it.
    send(&client, show(&added));
    wait(&client, "a tab chosen", |v| v.selected_tab.as_ref() == Some(&added));
    send(&client, edit(&session, Some(&added), Edit::Close));
    wait(&client, "a tab closed", |v| {
        v.tabs().len() == initial.tabs().len() && v.selected_tab.as_ref() == Some(&tab)
    });

    // A second window on the same session takes its panes by showing it: the
    // window looked at is the one they are to fit.
    let other = open(&endpoint, Herald::new().0);
    wait(&other, "another connection", |v| v.connected);
    send(&other, Action::SelectSession(session.clone()));
    wait(&other, "a session shown on request", drawn);
    // Each goes by this machine's name, which tells it from one on another.
    let hostname = std::fs::read_to_string("/proc/sys/kernel/hostname").unwrap();
    let clients = || {
        let listed = call("session.list_clients", json!({"session_id": session}));
        let clients = listed["clients"].as_array().unwrap().iter();
        let ours = clients.filter(|c| c["info"]["name"] == "Rexy");
        ours.filter(|c| c["info"]["label"] == hostname.trim()).count()
    };
    assert_eq!(clients(), 2);
    wait(&other, "a pane taken by being shown", owned);
    wait(&client, "a pane lost to another window", |v| !owned(v));
    // The first does not take it back for that, or the two would never be done.
    std::thread::sleep(Duration::from_millis(300));
    assert!(owned(&other.snapshot()) && !owned(&client.snapshot()));
    // Each takes it by using it, and it comes back when the other goes.
    send(&client, Action::FocusPane { pane: pane.clone() });
    wait(&client, "a pane taken back by using it", owned);
    wait(&other, "a pane lost in turn", |v| !owned(v));
    send(&other, Action::FocusPane { pane: pane.clone() });
    wait(&other, "a pane taken", owned);
    wait(&client, "a pane lost again", |v| !owned(v));
    // A connection made again finds the client still in the session it had joined,
    // and leaves it if that is no longer on show.
    send(&other, Action::Reconnect);
    other.leave();
    wait(&client, "a pane given back", owned);
    wait(&other, "another window connected again", |v| v.connected);
    assert_eq!(clients(), 1);
    assert!(other.snapshot().tabs().is_empty());
    let stays_left = || {
        std::thread::sleep(Duration::from_millis(400));
        let left = other.snapshot();
        assert!(left.connected && left.selected_session.is_none() && left.tabs().is_empty());
    };
    // It stays left when the server next reports a change.
    send(&client, Action::CreateTab(session.clone()));
    send(&client, show(&tab));
    stays_left();
    // And when it left before the server had done what it was last asked.
    send(&other, Action::SelectSession(session.clone()));
    other.leave();
    stays_left();
    send(&other, Action::CreateSession(Some("Third".into())));
    other.leave();
    let third = wait(&client, "a session made by a client that left", |v| {
        v.sessions.iter().any(|s| s.name == "Third")
    });
    stays_left();
    let third = third.sessions.iter().find(|s| s.name == "Third").unwrap();
    send(&client, edit(&third.id, None, Edit::Close));
    wait(&client, "that session ended", |v| v.sessions.iter().all(|s| s.name != "Third"));
    // The probe shows the first session as a window would, and sees its terminal.
    let probed = rexy::backend::probe(endpoint, Duration::from_secs(12)).unwrap();
    assert!(probed.contains("1 sessions") && !probed.contains(" 0 decoded"));
}

/// The keys follow the server's keymap, the server's own actions can be done, and
/// what other clients ask of this one reaches it and is answered.
#[test]
#[ignore = "requires REXY_TEST_SERVER pointing to a disposable Rex server"]
fn live_keymap_actions_and_requests() {
    let endpoint = endpoint();
    let call = |method: &str, payload: Value| call(&endpoint, method, payload);
    let client = open(&endpoint, Herald::new().0);
    let keymap = |label: &str, ready: &dyn Fn(&Value) -> bool| {
        let view = wait(&client, label, |v| v.keymap.as_deref().is_some_and(ready));
        view.keymap.unwrap()
    };
    let list = |value: &Value| value.as_array().cloned().unwrap_or_default();
    let bound = |map: &Value, keys: &Value, action: &str| {
        let is = |b: &Value| &b["keys"] == keys && b["action"] == action;
        list(&map["bindings"]).iter().any(is)
    };
    // With nothing in the configuration, this client's own keys come back as
    // they were sent: keys are matched by how they are spelled.
    let first = keymap("the server's keymap", &|_| true);
    let asked = rexy::ui::commands::request();
    let own = list(&asked["defaults"][0]["bindings"]);
    assert!(own.len() > 30 && list(&first["diagnostics"]).is_empty());
    for binding in &own {
        let action = binding["action"].as_str().unwrap();
        assert!(bound(&first, &binding["keys"], action), "{binding}");
    }
    // Written another way, each comes back in this spelling: the server reads
    // them all, but for the keys that copy and paste, which it keeps as they came.
    let mut other = asked.clone();
    for binding in other["defaults"][0]["bindings"].as_array_mut().unwrap() {
        let key = binding["keys"][0].as_str().unwrap();
        let (held, name) = key.split_at(key[..key.len() - 1].rfind('+').map_or(0, |at| at + 1));
        let mut held: Vec<String> = held.split_terminator('+').map(str::to_uppercase).collect();
        held.reverse();
        held.push(name.to_uppercase());
        binding["keys"] = json!([held.join("+")]);
    }
    let read = call("client.keymap", other).unwrap();
    for binding in &own {
        let action = binding["action"].as_str().unwrap();
        let read = bound(&read, &binding["keys"], action);
        let kept = ["[Copy]", "[Paste]"].map(Value::from).contains(&binding["keys"][0]);
        assert!(read != kept, "{binding}");
    }

    // What another client asks of this one reaches it, and the answer reaches
    // the other: that it was done, or that it was not and why.
    send(&client, client.snapshot().unshown().unwrap());
    let shown = wait(&client, "a terminal", drawn);
    let session = shown.selected_session.clone().unwrap();
    let pane = shown.current_tab().unwrap().panes[0].id.clone();
    let clients = call("client.list", json!({})).unwrap();
    let me = list(&clients["clients"]).into_iter();
    let me = me.filter(|c| c["info"]["name"] == "Rexy").map(|c| c["client_id"].clone());
    let me: Vec<Value> = me.collect();
    assert_eq!(me.len(), 1);
    let perform = |action: &str, error: Option<&'static str>| {
        let request =
            json!({"client_id": me[0], "action": action, "origin": "cli", "block_id": pane});
        std::thread::scope(|scope| {
            let asking = scope.spawn(|| call("client.perform", request));
            let mut asked = wait(&client, "another client's asking", |v| !v.asked.is_empty()).asked;
            let asked = asked.remove(0);
            assert_eq!((asked.name.as_str(), asked.pane.as_ref()), (action, Some(&pane)));
            let call = asked.call.unwrap();
            send(&client, Action::Reply { call, error });
            asking.join().unwrap()
        })
    };
    assert_eq!(perform("pane.zoom", None).unwrap()["performed_by"], me[0]);
    let refused = perform("pane.zoom", Some("refused")).unwrap_err();
    assert!(refused.contains("refused"), "{refused}");
    // A window that takes nothing, as one out of sight does not, refuses in time.
    let untaken = json!({"client_id": me[0], "action": "pane.zoom"});
    let untaken = call("client.perform", untaken).unwrap_err();
    assert!(untaken.contains("refused"), "{untaken}");
    // What is not among this client's actions, the server refuses for it.
    let unknown = json!({"client_id": me[0], "action": "client.settings"});
    let unknown = call("client.perform", unknown).unwrap_err();
    assert!(unknown.contains("not_supported"), "{unknown}");
    assert!(client.snapshot().asked.is_empty());

    // The account's configuration changes the keys, and the server says when: a
    // key rebound, a key freed, a sequence a function answers, an action and a mode.
    let path = first["config"]["path"].as_str().unwrap();
    // Only that of a server made for testing, as `tests/smoke.py` makes one.
    let disposable = ["/work/rexy-live-", "/work/rexy-smoke-"];
    assert!(disposable.iter().any(|made| path.contains(made)), "refusing to write {path}");
    // A second tab, for the configuration to have the server show.
    let shell = json!({"block": {"flavor": "com.superlogical.terminal.shell"}});
    let made = json!({"session_id": session, "layout": shell, "focus": false});
    let other = call("session.new_window", made).unwrap();
    let other = other["window_id"].as_str().unwrap().to_owned();
    let first = shown.selected_tab.clone().unwrap();
    let go = |name: &str, tab: &str| {
        format!(
            r#"rex.action{{ name = "{name}", title = "{name}", run = function(ctx, args)
          rex.session.focus_window{{ window_id = "{tab}" }}
        end }}"#
        )
    };
    let config = r#"
        rex.bind("ctrl+shift+d", "pane.zoom")
        rex.unbind("ctrl+tab")
        rex.bind("ctrl+x>ctrl+s", function(ctx, ev)
          rex.session.set_label{ label = "after " .. ev.prefix[1] }
          return true
        end)
        rex.action{
          name = "focus_mode", title = "Focus Mode", repeats = true,
          run = function(ctx, args) rex.client.queue("pane.zoom") end,
        }
        rex.mode("only", { exclusive = true })
        rex.bind("only/z", "client.mode.exit")
        rex.bind("ctrl+x>ctrl+t", function(ctx, ev) rex.invoke("to_other") return true end)
        rex.action{
          name = "ask", title = "Ask",
          run = function(ctx, args)
            local name = rex.ui.input{ title = "Name it", placeholder = "a name", value = "was" }
            if name == nil then
              rex.session.set_label{ label = "unanswered" }
              return
            end
            local which = rex.ui.pick{
              title = "Which", multiple = true, confirm = "Take",
              items = { "one", { id = "two", title = "Two", subtitle = "second", selected = true } },
            }
            rex.session.set_label{ label = name .. " " .. table.concat(which or {}, "+") }
          end,
        }
        rex.action{
          name = "far", title = "Far",
          run = function(ctx, args)
            local listed, why = rex.server("other").call("session.list", {})
            local said = listed and ("far has a " .. type(listed.sessions)) or ("far: " .. tostring(why))
            rex.session.set_label{ label = said }
          end,
        }
    "#;
    let config = format!("{config}\n{}\n{}", go("to_other", &other), go("to_first", &first));
    std::fs::write(path, config).unwrap();
    rex(&endpoint, &["config", "reload"]).unwrap();
    let zoom = json!(["ctrl+shift+d"]);
    let changed = keymap("a keymap read again", &|map| bound(map, &zoom, "pane.zoom"));
    let bindings = list(&changed["bindings"]);
    let at =
        |action: &str| bindings.iter().position(|b| b["keys"] == zoom && b["action"] == action);
    // The key's own binding is still there, to be tried after the new one.
    assert!(at("pane.zoom") < at("pane.split.right") && at("pane.zoom").is_some());
    assert!(!bindings.iter().any(|b| b["keys"] == json!(["ctrl+tab"])));
    assert!(bound(&changed, &json!(["ctrl+shift+tab"]), "client.tab.previous"));
    assert!(bound(&changed, &json!(["ctrl+x", "ctrl+s"]), "client.key.forward"));
    let mode = &changed["modes"][0];
    assert_eq!((&mode["name"], &mode["exclusive"]), (&json!("only"), &json!(true)));
    let in_mode = |b: &Value| b["mode"] == "only" && b["keys"] == json!(["z"]);
    assert!(bindings.iter().any(in_mode));
    let action = list(&changed["actions"]).into_iter();
    let action: Vec<_> = action.filter(|a| a["name"] == "focus_mode").collect();
    assert_eq!((&action[0]["title"], &action[0]["repeats"]), (&json!("Focus Mode"), &json!(true)));
    assert!(list(&changed["diagnostics"]).is_empty(), "{:#}", changed["diagnostics"]);

    // The last key of a sequence goes to the function bound to it with the keys
    // before it, and without them to the terminal.
    let mut last = key(&pane, "s", "key_s", model::CTRL, true);
    if let Action::Key { served, .. } = &mut last {
        *served = Some(vec!["ctrl+x".into()]);
    }
    send(&client, last);
    wait(&client, "a sequence answered by a function", |v| {
        v.session().is_some_and(|s| s.name == "after ctrl+x")
    });
    let typed = |v: &View| screen(v, &pane).matches('s').count();
    let before = typed(&client.snapshot());
    send(&client, key(&pane, "s", "key_s", 0, true));
    wait(&client, "a key typed", |v| typed(v) > before);
    enter(&client, &pane);

    // An action of the configuration's is done by the server, and what it asks
    // of this client in turn comes back to be done here.
    let perform = |name: &str| Action::Perform {
        name: name.into(),
        args: Value::Null,
        origin: "key",
        pane: Some(pane.clone()),
        servers: vec!["other".into()],
        far: None,
    };
    send(&client, perform("focus_mode"));
    let queued = wait(&client, "an action asked in turn", |v| !v.asked.is_empty()).asked;
    assert_eq!((queued[0].name.as_str(), &queued[0].call), ("pane.zoom", &None));
    // So is a terminal's own: this one clears what the terminal shows.
    let typed = |v: &View| screen(v, &pane).contains("rexy-typed");
    let text = "echo rexy-typed".into();
    send(&client, Action::Text { pane: pane.clone(), text });
    wait(&client, "something typed", typed);
    enter(&client, &pane);
    wait(&client, "a second line of it", |v| screen(v, &pane).matches("rexy-typed").count() == 2);
    send(&client, perform("com.superlogical.terminal.clear"));
    wait(&client, "a terminal cleared", |v| !typed(v));
    // What an action asks the user comes to be answered here: with a line of
    // text, and with some of the things it offers. It carries on with each.
    let named = |name: &'static str| move |v: &View| v.session().is_some_and(|s| s.name == name);
    send(&client, perform("ask"));
    let asked = wait(&client, "an action's question", |v| !v.questions.is_empty()).questions;
    let line = &asked[0];
    assert_eq!(
        (line.title.as_str(), line.hint.as_str(), line.value.as_str()),
        ("Name it", "a name", "was")
    );
    assert_eq!(line.choices, None);
    let answer = model::Answer::Text("named".into());
    send(&client, Action::Answer { call: line.call.clone(), answer });
    let asked = wait(&client, "its next question", |v| !v.questions.is_empty()).questions;
    let which = &asked[0];
    let offered = which.choices.as_ref().unwrap();
    let said =
        |at: usize| (offered[at].id.as_str(), offered[at].title.as_str(), offered[at].chosen);
    assert_eq!((said(0), said(1)), (("one", "one", false), ("two", "Two", true)));
    assert_eq!(
        (offered[1].under.as_str(), which.several, which.confirm.as_deref()),
        ("second", true, Some("Take"))
    );
    let answer = model::Answer::Chosen(vec!["one".into(), "two".into()]);
    send(&client, Action::Answer { call: which.call.clone(), answer });
    wait(&client, "an action that was answered", named("named one+two"));
    // And it is told when the user will not say.
    send(&client, perform("ask"));
    let asked = wait(&client, "a question again", |v| !v.questions.is_empty()).questions;
    let answer = model::Answer::Refused;
    send(&client, Action::Answer { call: asked[0].call.clone(), answer });
    wait(&client, "an action left unanswered", named("unanswered"));
    // An action that calls another server has this client make the call, and
    // goes on with what it is told came of it: here, by this same server.
    let relayed = |label: &str| {
        let start = Instant::now();
        loop {
            let (mut calls, mut called) = client.relays();
            if !calls.is_empty() || !called.is_empty() {
                return (calls.pop(), called.pop());
            }
            assert!(start.elapsed() < Duration::from_secs(15), "{label}");
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    send(&client, perform("far"));
    let asked = relayed("a call for another server").0.unwrap();
    assert_eq!((asked.server.as_str(), asked.method.as_str()), ("other", "session.list"));
    send(&client, Action::Call(asked.clone()));
    let came = relayed("what came of the call").1.unwrap();
    assert_eq!((&came.call, came.answer.is_ok()), (&asked.call, true));
    send(&client, Action::Called(came));
    wait(&client, "an action told of its call", named("far has a table"));
    // Or with why the call was not made.
    send(&client, perform("far"));
    let asked = relayed("a call again").0.unwrap();
    let answer = Err(["refused".into(), "no such server".into()]);
    send(&client, Action::Called(model::Called { call: asked.call, answer }));
    wait(&client, "an action told its call failed", named("far: no such server"));
    // One there is not is said to have failed.
    send(&client, perform("no_such_action"));
    let failed = wait(&client, "an action that failed", |v| v.error.is_some());
    // In the server's words for it, which name the action, and not by its code.
    let said = failed.error.unwrap();
    assert!(said.contains("no_such_action"), "{said}");
    assert!(!said.contains("not_found"), "{said}");
    assert_eq!(session, client.snapshot().selected_session.unwrap());

    // The tab on show is the window's own: another client that has the server
    // show the other tab does not move it.
    let tab = |v: &View| v.selected_tab.clone().unwrap();
    call("session.focus_window", json!({"session_id": session, "window_id": other})).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(tab(&client.snapshot()), first);
    // What the user has the server do moves it: a key that a function answers,
    // which shows the other tab, and an action, which shows the first again.
    let mut key = key(&pane, "t", "key_t", model::CTRL, true);
    if let Action::Key { served, .. } = &mut key {
        *served = Some(vec!["ctrl+x".into()]);
    }
    send(&client, key);
    wait(&client, "the tab a key's function showed", |v| tab(v) == other);
    send(&client, perform("to_first"));
    wait(&client, "the tab an action showed", |v| tab(v) == first);
    call("session.close_window", json!({"session_id": session, "window_id": other})).unwrap();
    // The configuration is left as it was found.
    std::fs::write(path, "").unwrap();
    rex(&endpoint, &["config", "reload"]).unwrap();
}

/// Files given to a pane reach its machine, and its terminal types where they
/// are: what was pasted is sent, and a file the server can read is not.
#[test]
#[ignore = "requires REXY_TEST_SERVER pointing to a disposable Rex server"]
fn live_files_reach_a_pane() {
    let endpoint = endpoint();
    let (session, pane, client) = shell(&endpoint, "files");
    // Wide, so that no path is broken over two lines.
    let size = Size { cols: 240, rows: 24, cell_width: 9, cell_height: 19 };
    send(&client, Action::Resize { pane: pane.clone(), size });
    let shows = |label: &str, text: &str| wait(&client, label, |v| screen(v, &pane).contains(text));
    let give = |files: Vec<File>| {
        send(&client, Action::Text { pane: pane.clone(), text: "cat ".into() });
        send(&client, Action::Upload { pane: pane.clone(), files });
    };
    let pasted = |name, text: &str| File::Pasted(name, Arc::from(text.as_bytes()));
    // One file, and then two, whose paths are typed together and in order.
    give(vec![pasted("alone.txt", "sent-alone\n")]);
    shows("the path of a file sent alone", "alone.txt");
    enter(&client, &pane);
    shows("what was sent alone", "\nsent-alone");
    // What is pasted is nowhere for the server to read, so a file here goes with it.
    let second = std::env::temp_dir().join(format!("rexy-live-{}", std::process::id()));
    std::fs::create_dir(&second).unwrap();
    let second = second.join("second.txt");
    std::fs::write(&second, "sent-second\n").unwrap();
    give(vec![pasted("first.txt", "sent-first\n"), File::Path(second.clone())]);
    let typed = shows("the paths of two files sent together", "second.txt");
    assert!(screen(&typed, &pane).contains("first.txt"));
    enter(&client, &pane);
    shows("what was sent together", "\nsent-first\nsent-second");
    assert!(!screen(&typed, &pane).contains(&second.display().to_string()));
    std::fs::remove_dir_all(second.parent().unwrap()).unwrap();
    // A large one arrives whole, in more than one message.
    let large = "0123456789abcdef".repeat(20_000) + "\nsent-large\n";
    send(&client, Action::Text { pane: pane.clone(), text: "tail -c 28 ".into() });
    send(&client, Action::Upload { pane: pane.clone(), files: vec![pasted("large.txt", &large)] });
    shows("the path of a large file", "large.txt");
    enter(&client, &pane);
    shows("the end of the large file", "\n0123456789abcdef\nsent-large");
    // A file on the server's own machine is named where it is, as one word.
    let here = std::env::temp_dir().join(format!("rexy live {}.txt", std::process::id()));
    std::fs::write(&here, "read-in-place\n").unwrap();
    give(vec![File::Path(here.clone())]);
    let typed = format!("cat '{}'", here.display());
    shows("the path of a file that is here already", &typed);
    enter(&client, &pane);
    shows("what was here already", "\nread-in-place");
    std::fs::remove_file(here).unwrap();
    // A folder is not sent, and the window is told why.
    give(vec![pasted("kept.txt", "kept"), File::Path(std::env::temp_dir())]);
    let failed = wait(&client, "word that nothing was sent", |v| v.error.is_some());
    assert!(failed.error.unwrap().starts_with("Nothing was sent to the pane"));
    // The server is left with the sessions it had, for the tests after this.
    rex(&endpoint, &["kill", &session]).unwrap();
}

#[test]
#[ignore = "requires REXY_TEST_SERVER pointing to a disposable Rex server"]
fn live_all_of_a_pane_is_copied_and_hosts_are_kept() {
    let endpoint = endpoint();
    let (session, pane, client) = shell(&endpoint, "output");
    // Lines too long for the pane, and more of them than one answer holds.
    let size = Size { cols: 60, rows: 24, cell_width: 9, cell_height: 19 };
    send(&client, Action::Resize { pane: pane.clone(), size });
    let line = ["abcdefghijklmnopqrstuvwxyz"; 4].join(" ");
    let text = format!("seq -f '%.0f {line}' 1 12000");
    send(&client, Action::Text { pane: pane.clone(), text });
    enter(&client, &pane);
    wait(&client, "the last of the output", |v| screen(v, &pane).contains("12000 abc"));
    send(&client, Action::CopyAll { pane: pane.clone() });
    let copied = wait(&client, "all of the pane", |v| v.output.is_some()).output.unwrap();
    assert!(copied.len() > 1024 * 1024, "{} bytes", copied.len());
    // Each line is as it was written, from the first to the last and once
    // each, and it is handed over once.
    assert!(copied.contains(&format!("\n1 {line}\n2 {line}\n")));
    assert!(copied.contains(&format!("\n11999 {line}\n12000 {line}\n")));
    assert_eq!(copied.lines().filter(|written| written.ends_with(&line)).count(), 12000);
    assert!(client.snapshot().output.is_none());

    // A host is added to the server's list, changed there and taken away.
    let host = |id: &str, label: &str| Host {
        id: id.into(),
        label: label.into(),
        endpoints: vec!["ssh://rexy-live.invalid".into()],
        ..Host::default()
    };
    let named = |view: &View, label: &str| view.hosts.iter().find(|h| h.label == label).cloned();
    send(&client, Action::PutHost(host("", "rexy-live")));
    let listed = wait(&client, "a host added", |v| named(v, "rexy-live").is_some());
    let id = named(&listed, "rexy-live").unwrap().id;
    assert!(!id.is_empty());
    send(&client, Action::PutHost(host(&id, "rexy-live-renamed")));
    let listed = wait(&client, "a host changed", |v| named(v, "rexy-live-renamed").is_some());
    assert_eq!(named(&listed, "rexy-live-renamed").unwrap().id, id);
    assert!(named(&listed, "rexy-live").is_none());
    // A second by the same name is refused, and the window is told.
    send(&client, Action::PutHost(host("", "rexy-live-renamed")));
    let failed = wait(&client, "word that a host was refused", |v| v.error.is_some());
    assert!(failed.error.unwrap().starts_with("The hosts are as they were"));
    send(&client, Action::RemoveHost(id));
    wait(&client, "a host removed", |v| named(v, "rexy-live-renamed").is_none());
    // The server is left with the sessions it had, for the tests after this.
    rex(&endpoint, &["kill", &session]).unwrap();
}
