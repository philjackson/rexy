//! A thin Rex client: one I/O runtime, bounded actions, separate terminal CPU workers.
//!
//! The model of a server is kept by following it. One request on connecting says
//! what sessions there are, and after that the server's news says what changes.
//! A session's view is asked for again only when news or an answer names a revision
//! of it newer than the one held. Nothing asked is waited for where input is taken:
//! each answer is dealt with when it comes.
use crate::{
    ghostty::Theme,
    model::{
        self, Action, Activity, Border, Division, Edit, Float, Host, Pane, Session, Tab, View, Wait,
    },
    notify::Herald,
    program::Program,
    ssh,
    terminal::{Notify, Request, Scroll, TerminalHandle},
    upload,
    wire::{self, Answer, Control, Ended, Event, Failed, News, Rpc},
};
use anyhow::{Context, Result, anyhow, bail, ensure};
use futures_util::{StreamExt, future::BoxFuture, stream::FuturesUnordered};
use serde_json::{Value, json};
use std::{
    cmp::Reverse,
    collections::{BTreeMap, HashMap, HashSet},
    convert::Infallible,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};

/// How long to wait before each try at something that keeps failing.
const RETRY: [Duration; 2] = [Duration::from_secs(2), Duration::from_secs(15)];
/// How long another client's asking waits for the window to take it.
const UNTAKEN: Duration = Duration::from_secs(3);
const FULL: [f32; 4] = [0., 0., 1., 1.];
const TERMINAL: &str = "com.superlogical.terminal";
const TITLE: &str = "com.superlogical.terminal.title";
const PROCESS: &str = "com.superlogical.terminal.process";
const STATUS: &str = "com.superlogical.terminal.program_status";
const THEME: &str = "com.superlogical.terminal.set_theme";
const COPIED: &str = "com.superlogical.terminal.clipboard_read";
const FORMAT: &str = "com.superlogical.terminal.format";
const BLOB: &str = "com.superlogical.terminal.blob_read";
/// How many tabs are kept fed at once, the one on show among them.
const WARM: usize = 16;
/// The most a program may copy, and the most of it to ask for at a time.
const MAX_COPY_BYTES: usize = 8 * 1024 * 1024;
/// The most of a pane's output to copy when the user asks for it all.
const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;
const COPY_PART_BYTES: usize = 960 * 1024;
/// What is said of a change to the host list that the server would not make.
const HOSTS_KEPT: &str = "The hosts are as they were";
static NULL: Value = Value::Null;

/// Coalesces repaint requests until the UI takes its next snapshot.
struct Wake {
    pending: AtomicBool,
    callback: Notify,
}
impl Wake {
    fn notify(&self) {
        if !self.pending.swap(true, Ordering::AcqRel) {
            (self.callback)();
        }
    }
}

#[derive(Default)]
struct State {
    view: View,
    /// The terminals being fed what their panes show, which take what is typed.
    terminals: HashMap<String, TerminalHandle>,
    /// Those fed no more, since a stream or the connection failed. Each that has
    /// drawn is kept for its last screen, to be read, looked through and copied
    /// from, until its pane is fed again or goes from sight. Nothing is typed
    /// into one, whether or not it had drawn.
    last: HashMap<String, Option<TerminalHandle>>,
    /// The user has asked for a session or a tab to be made, and for nothing
    /// else to be shown since. What is made is shown once it is there.
    awaited: bool,
    /// The calls an action of this server's configuration has this client
    /// make on another of the window's servers, and what has come of those it
    /// made on this server for home: each for the window to pass on.
    calls: Vec<model::Call>,
    called: Vec<model::Called>,
}

impl State {
    /// Count a pane's terminal among those fed no more. One that has drawn is
    /// kept for its last screen, as is the screen kept from an earlier time.
    fn rest(&mut self, pane: &str) {
        let terminal = self.terminals.remove(pane);
        let drawn = terminal.filter(|terminal| terminal.latest().is_some());
        // Its last screen is what it came to, though its pane was out of sight.
        drawn.iter().for_each(|terminal| terminal.show(true));
        let kept = self.last.entry(pane.into()).or_default();
        if drawn.is_some() {
            *kept = drawn;
        }
    }
}

#[derive(Clone)]
struct Shared {
    state: Arc<Mutex<State>>,
    wake: Arc<Wake>,
    herald: Herald,
}
impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn change(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.lock());
        self.wake.notify();
    }
    /// Give the window something to pass on to another of its servers. It is
    /// told every time: it passes them on out of sight too, where it draws
    /// nothing and so takes no snapshot.
    fn relay(&self, f: impl FnOnce(&mut State)) {
        f(&mut self.lock());
        (self.wake.callback)();
    }
    fn status(&self, message: impl Into<String>, connected: bool) {
        self.change(|s| {
            s.view.status = message.into();
            s.view.connected = connected;
        });
    }
    fn fail(&self, error: String) {
        self.change(|s| s.view.error = Some(error));
    }
    fn terminal(&self, pane: &str) -> Option<TerminalHandle> {
        self.lock().terminals.get(pane).cloned()
    }
}

pub struct Client {
    actions: mpsc::Sender<Action>,
    shared: Shared,
    /// Closes when the network worker has ended.
    done: flume::Receiver<()>,
}

impl Drop for Client {
    fn drop(&mut self) {
        // The worker stops when its queue closes, first telling a server it is
        // connected to that this client has gone. The process may be about to end,
        // so wait a moment for that to be said. With no connection there is nothing to say.
        drop(std::mem::replace(&mut self.actions, mpsc::channel(1).0));
        if self.shared.lock().view.connected {
            let wait = wire::CLOSE_TIMEOUT + Duration::from_millis(150);
            let _ = self.done.recv_timeout(wait);
        }
    }
}

impl Client {
    /// Connect to a server at the first of its endpoints that answers, and keep
    /// connected. None of its sessions is shown until one is asked for.
    /// What its panes have to say to someone not looking goes to `herald`.
    /// `keymap` is what the server's keymap is asked with: what a window can do
    /// itself, and by which keys. A client with none asks for no keymap. `theme`
    /// is the colours it gives the terminals it owns, as Rex's `set_theme` takes
    /// them, and as they come to be. One with none leaves each the colours it has.
    pub fn spawn(
        endpoints: Vec<String>,
        wake: Notify,
        herald: Herald,
        keymap: Option<Value>,
        theme: Theme,
    ) -> Self {
        let (ended, done) = flume::bounded(0);
        let shared = Shared {
            state: Arc::default(),
            wake: Arc::new(Wake { pending: AtomicBool::new(false), callback: wake }),
            herald,
        };
        let (actions, receiver) = mpsc::channel(256);
        let worker = shared.clone();
        let started = std::thread::Builder::new().name("rex-io".into()).spawn(move || {
            // Dropped last, once the runtime and all it carried are gone.
            let _ended = ended;
            match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime.block_on(run(endpoints, keymap, theme, receiver, worker)),
                Err(error) => {
                    worker.status(format!("Cannot start network runtime: {error}"), false)
                }
            }
        });
        if let Err(error) = started {
            shared.status(format!("Cannot start network worker: {error}"), false);
        }
        Self { actions, shared, done }
    }

    pub fn snapshot(&self) -> View {
        // Reset before reading: an update arriving during the copy schedules another paint.
        self.shared.wake.pending.store(false, Ordering::Release);
        let mut state = self.shared.lock();
        // What is told once is handed over, not copied: some of it may be long.
        let told = &mut state.view;
        let mut view = View {
            error: told.error.take(),
            made: told.made.take(),
            copied: told.copied.take(),
            output: told.output.take(),
            asked: std::mem::take(&mut told.asked),
            questions: std::mem::take(&mut told.questions),
            settled: std::mem::take(&mut told.settled),
            rang: std::mem::take(&mut told.rang),
            ..told.clone()
        };
        let screen = |pane: &mut Pane| {
            if let Some(terminal) = state.terminals.get(&pane.id) {
                pane.frame = terminal.latest();
                pane.status = match terminal.status() {
                    Some(error) => error,
                    None if pane.frame.is_some() => String::new(),
                    None => "Restoring terminal…".into(),
                };
            } else if let Some(Some(terminal)) = state.last.get(&pane.id) {
                // Its last screen, with what has been selected on it since.
                pane.frame = terminal.latest();
            }
        };
        view.panes_mut().for_each(screen);
        // One looked at from elsewhere has its screen wherever it is.
        if let Some(pane) = view.peeked.clone().and_then(|pane| view.find_mut(&pane)) {
            screen(pane);
        }
        view
    }

    pub fn try_send(&self, action: Action) -> Result<(), String> {
        // Queue under the state lock. A failed connection is marked disconnected
        // under the same lock before its queue is discarded, so input accepted
        // on it can never reach its replacement.
        let mut state = self.shared.lock();
        if !state.view.connected && !matches!(action, Action::Reconnect) {
            return Err("Disconnected; input was not sent".into());
        }
        // The session and the tab this asks to be shown, where it says which.
        let shown = match &action {
            Action::SelectSession(session) | Action::CreateTab(session) => {
                Some((session.clone(), None))
            }
            Action::SelectTab { session, tab } => Some((session.clone(), Some(tab.clone()))),
            _ => None,
        };
        let making = matches!(
            action,
            Action::CreateSession(_) | Action::CreateTab(_) | Action::MovePane { tab: None, .. }
        );
        self.actions.try_send(action).map_err(|e| format!("Input not sent: {e}"))?;
        if shown.is_some() || making {
            state.awaited = making;
        }
        // Show it at once. The server is told, and need not answer for that.
        let view = &mut state.view;
        if let Some((id, tab)) = shown
            && view.sessions.iter().any(|session| session.id == id)
        {
            if tab.is_some() || view.selected_session.as_ref() != Some(&id) {
                view.selected_tab = tab;
            }
            view.selected_session = Some(id);
            view.settle();
            drop(state);
            self.shared.wake.notify();
        }
        Ok(())
    }

    /// What there is to pass on to the window's other servers: the calls an
    /// action of this server's has this client make on one, and what came of
    /// those made on this server. Each is handed over once.
    pub fn relays(&self) -> (Vec<model::Call>, Vec<model::Called>) {
        let mut state = self.shared.lock();
        (std::mem::take(&mut state.calls), std::mem::take(&mut state.called))
    }

    /// The session and the tab that hold a pane, if this server has the pane.
    pub fn place(&self, pane: &str) -> Option<(String, String)> {
        let state = self.shared.lock();
        let (session, tab, _) = state.view.find(pane)?;
        Some((session.id.clone(), tab.id.clone()))
    }

    /// Stop showing this server's session. Its programs keep running there.
    pub fn leave(&self) {
        let mut state = self.shared.lock();
        state.awaited = false;
        state.view.selected_session = None;
        state.view.selected_tab = None;
        // Even with nothing on show: it may be in a session still, to be left.
        if state.view.connected {
            let _ = self.actions.try_send(Action::Shown);
        }
    }

    /// Look at a pane from wherever the window is, or with none look no more.
    pub fn peek(&self, pane: Option<&str>) {
        let mut state = self.shared.lock();
        state.view.peeked = pane.map(str::to_owned);
        if state.view.connected {
            let _ = self.actions.try_send(Action::Shown);
        }
    }

    /// The terminal of a pane that is fed, or the one kept for its last screen, for
    /// what is done to it here and never sent to the server: moving through its
    /// history, selecting, and finding text.
    pub fn terminal(&self, pane: &str) -> Result<TerminalHandle, String> {
        let state = self.shared.lock();
        let kept = || state.last.get(pane)?.as_ref();
        let terminal = state.terminals.get(pane).or_else(kept);
        terminal.cloned().ok_or_else(|| "Terminal is not ready".into())
    }
}

/// How long to wait before trying again what has failed with `error`, having failed
/// `failures` times running before, or `None` to try it no more unasked. Most is
/// tried for as long as it takes, slowly. What is reached by a `login` to another
/// machine is left alone after a few tries: one that keeps refusing a login may
/// come to refuse the address it comes from.
fn retry(error: &anyhow::Error, failures: usize, login: bool) -> Option<Duration> {
    // A server this client cannot talk to, and a terminal that has closed, will
    // be no different next time.
    if let Some(Ended::Incompatible(_) | Ended::BlockClosed) = error.downcast_ref() {
        return None;
    }
    let slowly = RETRY.last().filter(|_| !login);
    RETRY.get(failures).or(slowly).copied()
}

/// What outlasts a connection to a server, for the next to take up.
#[derive(Default)]
struct Link {
    /// The server knows this client again by it, in the session it had joined.
    token: Option<String>,
    /// The sessions this client has joined, each from asking to until told it has
    /// left: the one on show, and those of the other tabs kept fed.
    attached: Vec<String>,
    /// The address that answered last, of the several a host name can stand for.
    target: Option<wire::Target>,
    /// The server is on this machine, and so are the files it can read.
    local: bool,
    /// What is known of every terminal on the server, shown or not.
    blocks: HashMap<String, Block>,
    /// What each connection asks the server's keymap with, if this client has keys.
    keymap: Option<Value>,
    /// The colours this client gives the terminals it owns, if it has any.
    theme: Option<Value>,
}

async fn run(
    endpoints: Vec<String>,
    keymap: Option<Value>,
    mut theme: Theme,
    mut actions: mpsc::Receiver<Action>,
    shared: Shared,
) {
    let login = endpoints.iter().any(|endpoint| endpoint.starts_with("ssh://"));
    let mut link = Link { keymap, ..Link::default() };
    // Tries in a row that never connected.
    let mut failures = 0;
    loop {
        // A theme that came while there was no connection is taken up by this one.
        link.theme = theme.borrow_and_update().clone();
        shared.status("Connecting…", false);
        let result = run_connected(&endpoints, &mut link, &mut theme, &mut actions, &shared).await;
        if actions.is_closed() {
            return;
        }
        if shared.lock().view.connected {
            failures = 0;
        }
        let delay = match &result {
            // The user asked for it to be made again, which is done at once.
            Ok(()) => Some(Duration::ZERO),
            Err(error) => retry(error, failures, login),
        };
        failures += 1;
        shared.change(|s| {
            // Each last screen is kept for context, and no old input is sent again.
            let fed: Vec<String> = s.terminals.keys().cloned().collect();
            fed.iter().for_each(|pane| s.rest(pane));
            for pane in s.view.panes_mut() {
                pane.status = "Disconnected".into();
                pane.owner = false;
            }
            s.view.connected = false;
            // Whether it will be tried again unasked comes first: there is not
            // always room for all of why.
            s.view.status = match (result, delay) {
                (Ok(()), _) => "Reconnecting…".into(),
                (Err(error), Some(_)) => format!("Retrying · {error:#}"),
                (Err(error), None) => format!("{error:#}"),
            };
        });
        while actions.try_recv().is_ok() {} // Commands belong to the failed connection.
        // Any action is a request to try now.
        let asked = match delay {
            Some(delay) => tokio::time::timeout(delay, actions.recv()).await,
            None => Ok(actions.recv().await),
        };
        if let Ok(None) = asked {
            return;
        }
    }
}

/// Reach the server at the first endpoint that answers. An `ssh://` endpoint is
/// reached through a tunnel, which lasts as long as it is held.
async fn connect(endpoints: &[String], link: &mut Link) -> Result<(Option<ssh::Tunnel>, Control)> {
    let mut failure = anyhow!("This host has no address");
    for endpoint in endpoints {
        let attempt = async {
            let (tunnel, endpoint) = if endpoint.starts_with("ssh://") {
                let tunnel = ssh::Tunnel::open(endpoint).await?;
                let local = tunnel.endpoint();
                (Some(tunnel), local)
            } else {
                (None, endpoint.clone())
            };
            let (token, last) = (link.token.as_deref(), link.target.as_ref());
            let control = Control::connect(&endpoint, token, last).await?;
            anyhow::Ok((tunnel, control))
        };
        match attempt.await {
            Ok(connected) => return Ok(connected),
            Err(error) => failure = error,
        }
    }
    Err(failure)
}

async fn run_connected(
    endpoints: &[String],
    link: &mut Link,
    theme: &mut Theme,
    actions: &mut mpsc::Receiver<Action>,
    shared: &Shared,
) -> Result<()> {
    let opened = async move {
        let (tunnel, mut control) = connect(endpoints, link).await?;
        link.token = control.reconnect_token.take();
        link.target = Some(control.target.clone());
        link.local = tunnel.is_none() && matches!(control.target, wire::Target::Unix(_));
        let opened = Connection::open(&control, link, shared).await;
        // What the server would not answer is reason enough. Of a connection
        // that ended meanwhile, its worker alone knows why.
        if let Err(error) = &opened
            && !error.is::<Failed>()
        {
            return Err(wire::ended(&mut control.task).await);
        }
        anyhow::Ok((tunnel, control, opened?))
    };
    // A client that goes while its connection is being made has no use for it.
    // Nothing else is asked of one that is yet to be made but to make it.
    let gone = async { while actions.recv().await.is_some() {} };
    let (_tunnel, mut control, mut connection) = tokio::select! {
        opened = opened => opened?,
        () = gone => return Ok(()),
    };
    // What other clients ask of a window still connecting waits here, for the
    // window to be told of once it can do it.
    let (mut connected, mut waiting) = (false, Vec::new());
    loop {
        // The UI is told of the connection once the tabs of every session have come,
        // so that it is never shown half a server.
        if !connected && connection.views.values().all(|held| !held.asking) {
            connected = true;
            while actions.try_recv().is_ok() {} // Requests to try again have been answered.
            shared.change(|s| {
                s.view.status.clear();
                s.view.connected = true;
            });
            connection.show().await;
            waiting.drain(..).for_each(|asked| connection.pass(asked));
        }
        tokio::select! {
            // In this order: the server's news is taken before its answers, as it
            // sent them. An answer taken first would have older news follow it,
            // and be told of as if it were new.
            biased;
            // Its worker alone knows why a connection has ended. Nothing else here
            // ends one, so that this is what is said of it.
            error = wire::ended(&mut control.task) => return Err(error),
            Some(event) = control.events.recv() => match event {
                Event::Asked(asked) if !connected => waiting.push(asked),
                event => connection.event(event).await,
            },
            // Nothing comes of what was asked of a connection that has ended.
            Some(Some((reply, answer))) = connection.replies.next() => {
                connection.answered(reply, answer).await;
            }
            action = actions.recv(), if connected => {
                let Some(action) = action else {
                    // The client is going. The server need not keep its place.
                    control.close().await;
                    return Ok(());
                };
                if matches!(action, Action::Reconnect) {
                    return Ok(());
                }
                if let Err(error) = connection.perform(action).await {
                    shared.fail(format!("{error:#}"));
                }
            }
            // Ghostty's colours, changed. A change while connecting is taken up
            // once connected, when the panes made meanwhile are fed.
            Ok(()) = theme.changed(), if connected => {
                let theme = theme.borrow_and_update().clone();
                connection.retheme(theme).await;
            }
        }
    }
}

/// What an answer on its way from the server is to.
enum Reply {
    /// Something asked of the server that has nothing to say for itself unless
    /// it failed, which is then said in these words: input, for one.
    Told(&'static str),
    /// A change to a session, or the making of one, whose answer says what revision
    /// the session is left at. With it are the session, a pane it brings out, and
    /// whether it makes a session or a tab, which is then to be shown.
    Change(String, Option<String>, bool),
    /// A session's view.
    View(String),
    /// A session whose view did not come, when it is time to ask for it again.
    /// No answer of the server's: the wait is over.
    Unseen(String),
    /// Joining a session, and leaving one.
    Joined(String),
    Left(String),
    /// The host list.
    Hosts,
    /// What this client's keys do.
    Keymap,
    /// Something the server did for the user: one of its own actions, or a key
    /// that a function of its configuration answers. It may ask this client for
    /// more. With it are the session and the tab that were on show when it was
    /// asked, to tell whether it has moved the server's tab.
    Performed(Option<(String, String)>),
    /// A call made here for an action of home's configuration: what home is
    /// answered by, and what was on show, as for an action of this server's own.
    Relayed(String, Option<(String, String)>),
    /// The view of that session as the server left it.
    Moved(String, String),
    /// A tab made for a pane of that session to have to itself. The server
    /// makes no tab without a terminal in it, so this one has a shell to stand
    /// in: the pane is moved beside that, and the shell then closed.
    Room(String, String),
    /// The pane is there, or is not. With it are the session, the pane, the
    /// shell that stood in for it and the tab.
    Housed(String, String, String, String),
    /// A terminal's title, its processes and what its programs report, in a list.
    /// `known` says an earlier connection knew the terminal.
    Learned {
        block: String,
        known: bool,
    },
}

/// The view held of a session, by how current it is.
#[derive(Default)]
struct Held {
    /// Its revision, once there is one.
    revision: Option<u64>,
    /// The newest revision heard of.
    heard: u64,
    /// A view has been asked for and has yet to come.
    asking: bool,
    /// How many times running a view asked for has not come.
    failures: usize,
}

impl Held {
    /// Whether the view held is as new as any heard of.
    fn current(&self) -> bool {
        self.revision.is_some_and(|held| held >= self.heard)
    }

    /// Hear of a revision, and say whether to ask for the view: when the one held
    /// is older, and none is on its way that may prove not to be.
    fn want(&mut self, revision: u64) -> bool {
        self.heard = self.heard.max(revision);
        let ask = !self.asking && !self.current();
        self.asking |= ask;
        ask
    }

    /// Be handed a view, and say whether to take it: when it is newer than the one
    /// held. Views may come in any order.
    fn take(&mut self, revision: u64) -> bool {
        let newer = self.revision.is_none_or(|held| revision > held);
        if newer {
            self.revision = Some(revision);
        }
        newer
    }
}

/// One control connection, and where it stands with the server. Dropping it stops
/// every terminal stream.
struct Connection<'a> {
    shared: &'a Shared,
    link: &'a mut Link,
    rpc: Rpc,
    client_id: String,
    /// The address that answered, of the several a host name can stand for.
    /// Terminal streams go straight to it.
    target: wire::Target,
    /// The answers on their way.
    replies: FuturesUnordered<BoxFuture<'static, Option<(Reply, Answer)>>>,
    /// The sessions there are, and how current the view of each is.
    views: HashMap<String, Held>,
    /// The terminals asked about on this connection. `Some` holds the news of one
    /// that came before the answers, which may be older than it, to be taken in
    /// after them.
    asking: HashMap<String, Option<Vec<News>>>,
    /// The hash of the host list held, and of the configuration that the keymap
    /// held came of.
    hosts_hash: String,
    keymap_hash: String,
    /// The session that a request to join or to leave is on its way for.
    joining: Option<String>,
    /// A session this client has made, or a tab of a session: shown once it is
    /// listed, if the user has asked for nothing else to be shown since.
    making: Option<(String, Option<String>)>,
    /// A pane this client has made or brought out, reported once it is on show.
    made: Option<String>,
    /// The shells that stand in tabs made for panes to move to, each with its
    /// session. A tab is none of the window's while one is in it.
    standing: Vec<(String, String)>,
    /// The tabs kept fed, the last shown first, each by its session and itself.
    /// Going back to one finds it as it is, with nothing to fetch.
    warm: Vec<(String, String)>,
    /// One stream per terminal fed, and whether what it shows is drawn.
    streams: HashMap<String, (JoinHandle<()>, Arc<AtomicBool>)>,
    /// The session last on show, whose panes are this client's to size.
    held: Option<String>,
    /// The panes on show. One that is another client's is taken as it comes on
    /// show, and not again while it stays there: two windows that show one tab
    /// would otherwise take it from each other in turn. After that it is taken
    /// by typing or clicking in it.
    showing: HashSet<String>,
    /// The fetching of the last text a program copied.
    copying: Option<JoinHandle<()>>,
    /// Where borders and floating layers were last asked to go, for `shape`
    /// to ask of the server.
    shaping: watch::Sender<Vec<(&'static str, Value)>>,
}

#[derive(Default)]
struct Block {
    session: String,
    title: String,
    /// The process of the shell it started with, if it started with one, and
    /// that shell's name. Where it is the one in the foreground, the shell is at
    /// its prompt.
    shell: Option<u64>,
    prompt: String,
    /// What is in its foreground, in the directory last known of it. `home` is
    /// the directory its user starts in.
    running: wire::Running,
    home: String,
    /// How far along a program in it says it is, by the terminal's own means.
    progress: Option<u8>,
    /// What its programs report of themselves, each record by its name. The
    /// program's own has none, and so comes first, and each part's after its whole.
    records: BTreeMap<String, Value>,
}

impl Block {
    /// What its programs say they are doing: the most pressing of it, the line
    /// said of that, and the icon of one still at it.
    fn says(&self) -> (Activity, &str, Option<Program>) {
        let (activity, record, app) = reported(self.records.values());
        (activity, record["msg"].as_str().unwrap_or_default(), app)
    }

    /// What it has to say to someone not looking, if that is other than it was `before`.
    fn news(&self, before: Activity) -> Option<String> {
        let (activity, message, _) = self.says();
        let word = activity.word().filter(|_| activity != before)?;
        Some(model::line(word, message))
    }

    /// Take in what is now in its foreground. A process whose directory cannot
    /// be read, as another user's cannot, leaves the one last known.
    fn run(&mut self, running: wire::Running) {
        let last = self.running.folder.take();
        self.running = wire::Running { folder: running.folder.or(last), ..running };
    }

    /// Take in news of it.
    fn note(&mut self, news: News) {
        match news {
            News::Title(title) => self.title = title,
            News::Program(running) => self.run(running),
            News::Status(records) => {
                for (id, record) in records {
                    match record {
                        Some(record) => self.records.insert(id, record),
                        None => self.records.remove(&id),
                    };
                }
            }
            News::Progress(progress) => self.progress = progress,
            // News of a moment, which changes nothing that is kept.
            News::Copied { .. } | News::Bell | News::Notice { .. } => {}
        }
    }
}

impl Drop for Connection<'_> {
    fn drop(&mut self) {
        let streams = self.streams.values().map(|(task, _)| task);
        for task in streams.chain(&self.copying) {
            task.abort();
        }
        // A terminal whose answers never came is not one the next connection knows.
        let unanswered = |id: &String| matches!(self.asking.get(id), Some(Some(_)));
        self.link.blocks.retain(|id, _| !unanswered(id));
    }
}

impl<'a> Connection<'a> {
    /// Begin on a new connection: learn what sessions there are, join the one on
    /// show, and ask for what the server's news will not tell.
    async fn open(control: &Control, link: &'a mut Link, shared: &'a Shared) -> Result<Self> {
        let rpc = control.rpc.clone();
        let shown = shared.lock().view.selected_session.clone();
        // A client the server knows again is still in the sessions it had joined,
        // and owns what it owned there. It leaves each unless it is still the one
        // on show: no tab is kept fed from one connection to the next. A new
        // client is in none.
        let before = std::mem::replace(&mut link.attached, shown.iter().cloned().collect());
        for before in before.iter().filter(|_| control.resumed) {
            if Some(before) != shown.as_ref() {
                rpc.call("session.detach", json!({"session_id": before})).await?;
            }
        }
        // Events of the whole server, so that tabs not on show keep their titles too.
        let request =
            json!({"event_scope": "all", "attach_session_ids": shown.iter().collect::<Vec<_>>()});
        let boot = rpc.call("session.bootstrap", request).await?;
        let joined = &boot["attached_sessions"][0]["view"];
        let listed = items(&boot["attach_all"]["sessions"]).filter_map(|session| {
            Some((session["session_id"].as_str()?, session["label"].as_str()))
        });
        let views: HashMap<_, _> =
            listed.clone().map(|(id, _)| (id.to_owned(), Held::default())).collect();
        link.blocks.retain(|_, block| views.contains_key(&block.session));
        shared.change(|s| {
            let mut held = std::mem::take(&mut s.view.sessions);
            for (id, name) in listed {
                // One held from before keeps its tabs until they are seen anew.
                let at = held.iter().position(|session| session.id == id);
                let known = at.map(|at| held.swap_remove(at)).unwrap_or_default();
                s.view.sessions.push(Session {
                    id: id.into(),
                    name: label(name, "Untitled"),
                    ..known
                });
            }
        });
        let mut connection = Self {
            shared,
            link,
            rpc,
            client_id: control.client_id.clone(),
            target: control.target.clone(),
            replies: FuturesUnordered::new(),
            views,
            asking: HashMap::new(),
            hosts_hash: String::new(),
            keymap_hash: String::new(),
            joining: None,
            making: None,
            made: None,
            standing: Vec::new(),
            warm: Vec::new(),
            streams: HashMap::new(),
            held: None,
            showing: HashSet::new(),
            copying: None,
            shaping: shape(control.rpc.clone(), shared.clone()),
        };
        match shown {
            Some(shown) if joined.is_object() => connection.view(&shown, joined).await,
            // The session on show has ended, or there was none.
            _ => connection.link.attached.clear(),
        }
        let sessions: Vec<String> = connection.views.keys().cloned().collect();
        for session in sessions {
            connection.want(&session, 0).await;
        }
        connection.ask(Reply::Hosts, "hosts.list", json!({})).await;
        connection.ask_keymap().await;
        Ok(connection)
    }

    /// Ask the server something and go on. Its answer comes to `answered`, with
    /// `reply` to say what it is to.
    async fn ask(&mut self, reply: Reply, method: &str, payload: Value) {
        // A connection that has ended takes no more, as the loop will find.
        if let Ok(response) = self.rpc.enqueue(method, payload).await {
            let answer = async move { Some((reply, response.await.ok()?)) };
            self.replies.push(Box::pin(answer));
        }
    }

    /// Ask what this client's keys do: its own, as changed by the account's
    /// configuration.
    async fn ask_keymap(&mut self) {
        if let Some(request) = self.link.keymap.clone() {
            self.ask(Reply::Keymap, "client.keymap", request).await;
        }
    }

    /// Ask it several things, to be answered together: by a list, with nothing in
    /// the place of what it would not say.
    async fn ask_all(&mut self, reply: Reply, requests: [(&str, Value); 3]) {
        let mut responses = Vec::new();
        for (method, payload) in requests {
            let Ok(response) = self.rpc.enqueue(method, payload).await else {
                return;
            };
            responses.push(response);
        }
        self.replies.push(Box::pin(async move {
            let mut said = Vec::new();
            for response in responses {
                said.push(response.await.ok()?.unwrap_or_default());
            }
            Some((reply, Ok(said.into())))
        }));
    }

    /// Ask for a session's view, if a revision of it is heard of that is newer than
    /// the one held.
    async fn want(&mut self, session: &str, revision: u64) {
        let held = self.views.get_mut(session);
        if held.is_some_and(|held| held.want(revision)) {
            let request = json!({"session_id": session});
            self.ask(Reply::View(session.into()), "session.view", request).await;
        }
    }

    /// Take a view of a session, if it is newer than the one held: its tabs and
    /// panes, and the terminals in them.
    async fn view(&mut self, session: &str, view: &Value) {
        let revision = view["revision"].as_u64().unwrap_or(0);
        let held = self.views.get_mut(session);
        if !held.is_some_and(|held| held.take(revision)) {
            return;
        }
        let mut terminals = Vec::new();
        let mut tabs = parse_tabs(view, &self.client_id, &mut terminals);
        // A tab being made for a pane is left out until the shell that stands in
        // it has closed: it is then the pane's alone, and is shown so. The
        // shell is never drawn, nor asked anything. One the view no longer
        // has is closed, and forgotten.
        self.standing.retain(|(held, shell)| held != session || terminals.contains(shell));
        let stands = |id: &String| self.standing.iter().any(|(_, shell)| shell == id);
        tabs.retain(|tab| !tab.every().any(|pane| stands(&pane.id)));
        terminals.retain(|id| !stands(id));
        // Terminals that have left it are forgotten, and those not seen before on
        // this connection asked about: news of them may have been missed. There is
        // no asking all of it at once, nor of several terminals.
        let blocks = &mut self.link.blocks;
        blocks.retain(|id, block| block.session != session || terminals.contains(id));
        self.asking.retain(|id, _| blocks.contains_key(id));
        for id in terminals {
            let known = self.link.blocks.contains_key(&id);
            self.link.blocks.entry(id.clone()).or_default().session = session.into();
            if !self.asking.contains_key(&id) {
                let request = json!({"session_id": session, "block_id": id, "args": {}});
                let asked = [TITLE, PROCESS, STATUS];
                let asked = asked.map(|method| (method, request.clone()));
                self.asking.insert(id.clone(), Some(Vec::new()));
                let reply = Reply::Learned { block: id, known };
                self.ask_all(reply, asked).await;
            }
        }
        self.shared.change(|s| {
            if let Some(held) = s.view.sessions.iter_mut().find(|held| held.id == session) {
                held.name = label(view["label"].as_str(), "Untitled");
                held.active = view["active_window_id"].as_str().map(str::to_owned);
                held.tabs = tabs;
            }
            fill(&mut s.view.sessions, &self.link.blocks);
        });
    }

    /// Take in what the server says has changed.
    async fn event(&mut self, event: Event) {
        match event {
            Event::Viewed { session, revision } => return self.want(&session, revision).await,
            Event::Hosts { hash } => {
                if hash != self.hosts_hash {
                    self.ask(Reply::Hosts, "hosts.list", json!({})).await;
                }
                return;
            }
            Event::Keymap { hash } => {
                if hash != self.keymap_hash {
                    self.ask_keymap().await;
                }
                return;
            }
            Event::Asked(asked) => return self.pass(asked),
            Event::Call(call) => return self.shared.relay(|s| s.calls.push(call)),
            Event::Question(question) => {
                return self.shared.change(|s| s.view.questions.push(Arc::new(question)));
            }
            Event::Settled { call } => return self.shared.change(|s| s.view.settled.push(call)),
            Event::Block { block, news } => {
                return match news {
                    News::Bell => self.rang(block),
                    News::Notice { title, body } => self.tell(&block, &model::line(&title, &body)),
                    News::Copied { text, id } => self.copied(block, text, id),
                    news => match self.asking.get_mut(&block) {
                        Some(Some(held)) => held.push(news),
                        _ => self.note(&block, true, |terminal| terminal.note(news)),
                    },
                };
            }
            // Its name is told, and its tabs are to be asked for.
            Event::Created { ref session, .. } => {
                self.views.entry(session.clone()).or_default();
                self.want(session, 0).await;
            }
            Event::Destroyed { ref session } => {
                self.views.remove(session);
                self.link.blocks.retain(|_, block| &block.session != session);
            }
            Event::Moved { .. } => {}
        }
        self.shared.change(|s| list(&mut s.view.sessions, &event));
        self.show().await;
    }

    /// Tell the window of something another client asks of it. A window that is out
    /// of sight takes nothing. What it has not taken after a while is refused for
    /// it, and so never done late.
    fn pass(&self, asked: model::Asked) {
        let (shared, rpc, call) = (self.shared.clone(), self.rpc.clone(), asked.call.clone());
        self.shared.change(|s| s.view.asked.push(asked));
        tokio::spawn(async move {
            tokio::time::sleep(UNTAKEN).await;
            let waits = |held: &mut model::Asked| held.call == call;
            let untaken = shared.lock().view.asked.extract_if(.., waits).count() > 0;
            if let Some(call) = call.filter(|_| untaken) {
                let _ = rpc.enqueue("client.reply", reply(&call, outcome(Some("refused")))).await;
            }
        });
    }

    /// Take the server's answer to something asked of it.
    async fn answered(&mut self, reply: Reply, answer: Answer) {
        match (reply, answer) {
            (Reply::Told(_), Ok(_)) => return,
            (Reply::Told(failed), Err(error)) => {
                return self.shared.fail(format!("{failed}: {error}"));
            }
            (Reply::Hosts, answer) => {
                // A server too old to keep a host list has none, and a list that
                // did not come leaves the one held: servers are dropped by it.
                let Ok(listed) = answer else {
                    return;
                };
                let hosts = items(&listed["hosts"]).filter_map(|host| {
                    let endpoints = items(&host["endpoints"]).filter_map(Value::as_str);
                    let text = |key: &str| host[key].as_str().unwrap_or_default().to_owned();
                    Some(Host {
                        id: text("id"),
                        label: host["label"].as_str()?.into(),
                        endpoints: endpoints.map(str::to_owned).collect(),
                        source: text("source"),
                    })
                });
                let hosts = hosts.collect();
                self.hosts_hash = listed["hash"].as_str().unwrap_or_default().into();
                return self.shared.change(|s| s.view.hosts = hosts);
            }
            (Reply::Keymap, Ok(keymap)) => {
                self.keymap_hash = keymap["config"]["hash"].as_str().unwrap_or_default().into();
                return self.shared.change(|s| s.view.keymap = Some(Arc::new(keymap)));
            }
            // This client's own keys go on as they are.
            (Reply::Keymap, Err(error)) => {
                return self.shared.fail(format!("The server's keys were not read: {error}"));
            }
            (Reply::Performed(shown), Ok(done)) => {
                // Its configuration may ask this client for actions of its own, in turn.
                let asked = items(&done["client_actions"]).filter_map(|asked| {
                    Some(model::Asked {
                        call: None,
                        name: asked["action"].as_str()?.into(),
                        args: asked["args"].clone(),
                        session: None,
                        pane: None,
                    })
                });
                let asked: Vec<_> = asked.collect();
                self.shared.change(|s| s.view.asked.extend(asked));
                // A key that no function took after all has done nothing here.
                return self.moved(shown.filter(|_| done["handled"] != false)).await;
            }
            (Reply::Performed(_), Err(error)) => return self.shared.fail(error.to_string()),
            (Reply::Relayed(call, shown), answer) => {
                // What it did may have moved this server's tab, as an action of
                // this server's own may: seen to once home has its answer.
                let shown = shown.filter(|_| answer.is_ok());
                let answer = answer.map_err(|error| match error {
                    Failed::Refused { code, message } => [code, message],
                    other => ["internal".to_owned(), other.to_string()],
                });
                self.shared.relay(|s| s.called.push(model::Called { call, answer }));
                return self.moved(shown).await;
            }
            // What the user had the server do has moved its tab, and the
            // window goes with it: the server's own doing does not move it.
            (Reply::Moved(session, tab), answer) => {
                if let Ok(view) = answer {
                    self.view(&session, &view).await;
                    self.shared.change(|s| s.view.follow(&session, &tab));
                }
            }
            (Reply::Learned { block, known }, answer) => {
                // Unless the terminal has gone meanwhile.
                let Some(held) = self.asking.get_mut(&block).and_then(Option::take) else {
                    return;
                };
                let said = answer.unwrap_or_default();
                let [title, process, status] = [0, 1, 2].map(|at| &said[at]);
                let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
                // How a terminal stood when first seen is not news. How one that the
                // last connection knew has changed since that was lost is.
                return self.note(&block, known, |terminal| {
                    terminal.title = text(&title["title"]);
                    terminal.shell = wire::shell(&process["child"]);
                    terminal.prompt = text(&process["child"]["name"]);
                    terminal.home = text(&process["home"]);
                    terminal.run(wire::running(&process["foreground"]));
                    let records = items(&status["records"]);
                    terminal.records =
                        records.map(|record| (text(&record["id"]), record.clone())).collect();
                    held.into_iter().for_each(|news| terminal.note(news));
                });
            }
            (Reply::Change(session, made, new), answer) => match answer {
                Ok(changed) => {
                    // A session it made is shown once listed, a tab once its
                    // session has it, and a pane once on show.
                    let text = |value: &Value| value.as_str().map(str::to_owned);
                    if new {
                        let tab = (session.clone(), text(&changed["window_id"]));
                        let made = text(&changed["session_id"]).map(|id| (id, None));
                        self.making = Some(made.unwrap_or(tab));
                    }
                    self.made = made.or(text(&changed["block_ids"][0])).or(self.made.take());
                    let revision = changed["revision"].as_u64().unwrap_or(0);
                    self.want(&session, revision).await;
                }
                Err(error) => self.shared.fail(error.to_string()),
            },
            (Reply::Room(session, pane), answer) => match answer {
                Ok(made) => {
                    let text = |value: &Value| value.as_str().unwrap_or_default().to_owned();
                    let (shell, tab) = (text(&made["block_ids"][0]), text(&made["window_id"]));
                    self.standing.push((session.clone(), shell.clone()));
                    let request = json!({
                        "session_id": session,
                        "block_id": pane,
                        "anchor_block_id": shell,
                    });
                    let reply = Reply::Housed(session, pane, shell, tab);
                    self.ask(reply, "session.move_block", request).await;
                }
                Err(error) => self.shared.fail(error.to_string()),
            },
            (Reply::Housed(session, pane, shell, tab), answer) => {
                // The tab is shown once it is the pane's alone, and the pane
                // given the keyboard. Failing that, it goes with its shell.
                match answer {
                    Ok(_) => {
                        self.making = Some((session.clone(), Some(tab)));
                        self.made = Some(pane);
                    }
                    Err(error) => self.shared.fail(error.to_string()),
                }
                let request = json!({"session_id": session, "block_id": shell});
                self.ask(Reply::Change(session, None, false), "block.close", request).await;
            }
            (Reply::View(session), answer) => {
                // A session that has ended has none, as the news says or will say.
                if let Some(held) = self.views.get_mut(&session) {
                    held.asking = false;
                    // How many times running its view had not come before this.
                    let failures = std::mem::take(&mut held.failures);
                    match answer {
                        Ok(view) => {
                            self.view(&session, &view).await;
                            // A newer one may have been heard of while this was on its way.
                            self.want(&session, 0).await;
                        }
                        Err(error) if error.is("session_not_found") => {}
                        // Its tabs are not known for any other reason. That is said
                        // once, and they are asked for again, soon and then seldom,
                        // if nothing else has asked by then.
                        Err(error) => {
                            held.failures = failures + 1;
                            if failures == 0 {
                                self.shared
                                    .fail(format!("A session's tabs were not read: {error}"));
                            }
                            let wait = RETRY[failures.min(RETRY.len() - 1)];
                            self.replies.push(Box::pin(async move {
                                tokio::time::sleep(wait).await;
                                Some((Reply::Unseen(session), Ok(Value::Null)))
                            }));
                        }
                    }
                }
            }
            (Reply::Unseen(session), _) => self.want(&session, 0).await,
            (Reply::Joined(session), answer) => {
                self.joining = None;
                match answer {
                    Ok(joined) => self.view(&session, &joined["view"]).await,
                    Err(error) => {
                        // Not in it after all, and not to be shown as if it were.
                        self.link.attached.retain(|held| *held != session);
                        self.shared.change(|s| {
                            if s.view.selected_session.as_ref() == Some(&session) {
                                s.view.selected_session = None;
                            }
                            // Nor is a pane of it looked at from elsewhere.
                            if s.view.looked().is_some_and(|(held, ..)| held.id == session) {
                                s.view.peeked = None;
                            }
                            s.view.error = Some(error.to_string());
                        });
                    }
                }
            }
            (Reply::Left(session), _) => {
                self.joining = None;
                self.link.attached.retain(|held| *held != session);
            }
        }
        self.show().await;
    }

    /// Bring what is on show, the session joined and the terminals streamed into
    /// line with the model and with what the user last asked for.
    async fn show(&mut self) {
        {
            let mut state = self.shared.lock();
            let State { view, awaited, .. } = &mut *state;
            let listed = |(session, tab): &mut (String, Option<String>)| {
                let has = |held: &Session| {
                    tab.as_ref().is_none_or(|tab| held.tabs.iter().any(|open| &open.id == tab))
                };
                view.sessions.iter().any(|held| held.id == *session && has(held))
            };
            if let Some((session, tab)) = self.making.take_if(listed)
                && *awaited
            {
                (view.selected_session, view.selected_tab) = (Some(session), tab);
            }
            view.settle();
            // In the tab on show: a pane moved to another tab is somewhere all along.
            let shown = view.current_tab();
            let seen = |id: &String| shown.is_some_and(|tab| tab.panes.iter().any(|p| &p.id == id));
            if let Some(made) = self.made.take_if(|made| seen(made)) {
                view.made = Some(made);
            }
        }
        self.shared.wake.notify();
        self.join().await;
        self.reconcile().await;
    }

    /// Whether this client is in a session: it has asked to join, and been answered.
    fn joined(&self, session: &str) -> bool {
        let attached = self.link.attached.iter().any(|held| held == session);
        attached && self.joining.as_deref() != Some(session)
    }

    /// Join the session on show, and leave each that has no tab kept fed, a
    /// request at a time: asked together, a leaving and a joining of one session
    /// may be taken in either order. The joining comes first, which is waited for.
    async fn join(&mut self) {
        if self.joining.is_some() {
            return;
        }
        // The session on show, and then that of a pane looked at from elsewhere.
        let wanted: Vec<String> = {
            let view = &self.shared.lock().view;
            let peeked = view.looked().map(|(session, ..)| &session.id);
            view.selected_session.iter().chain(peeked).cloned().collect()
        };
        let attached = &mut self.link.attached;
        let kept = |session: &String| {
            wanted.contains(session) || self.warm.iter().any(|(held, _)| held == session)
        };
        let (reply, method, session) = match wanted.iter().find(|new| !attached.contains(new)) {
            Some(new) => {
                attached.push(new.clone());
                (Reply::Joined(new.clone()), "session.attach", new.clone())
            }
            None => match attached.iter().find(|held| !kept(held)) {
                Some(old) => (Reply::Left(old.clone()), "session.detach", old.clone()),
                None => return,
            },
        };
        self.joining = Some(session.clone());
        self.ask(reply, method, json!({"session_id": session})).await;
    }

    /// Stream the terminals of the tab on show, once its session is joined, and
    /// go on streaming those of the tabs shown before it, up to `WARM` in all.
    /// One that comes on show while another client's is taken from it. A pane
    /// looked at from elsewhere is streamed and drawn too, and is not taken.
    async fn reconcile(&mut self) {
        let (selected, shown, kept) = {
            let mut state = self.shared.lock();
            let view = &state.view;
            // Only a session this connection has joined can be streamed.
            let tab = view.current_tab().map(|tab| tab.id.clone());
            let shown = view.selected_session.clone().zip(tab);
            let shown = shown.filter(|(session, _)| self.joined(session));
            if let Some(shown) = &shown {
                self.warm.retain(|held| held != shown);
                self.warm.insert(0, shown.clone());
            }
            // A tab that has closed is kept no more, nor one of a session left.
            let attached = &self.link.attached;
            let open = |session: &String, tab: &String| {
                let there = |(held, open): (&Session, &Tab)| held.id == *session && open.id == *tab;
                attached.contains(session) && view.every_tab().any(there)
            };
            self.warm.retain(|(session, tab)| open(session, tab));
            self.warm.truncate(WARM);
            // Each terminal of theirs, with its session, whether it is drawn,
            // whether it is on show, and whether it is this client's.
            let mut kept = HashMap::new();
            for (session, tab) in view.every_tab() {
                let held = (session.id.clone(), tab.id.clone());
                if self.warm.contains(&held) {
                    let on_show = shown.as_ref() == Some(&held);
                    let terminals =
                        tab.panes.iter().filter(|pane| self.link.blocks.contains_key(&pane.id));
                    kept.extend(terminals.map(|pane| {
                        (pane.id.clone(), (held.0.clone(), on_show, on_show, pane.owner))
                    }));
                }
            }
            // The pane looked at from elsewhere is drawn wherever it is, and is
            // no more on show for that.
            if let Some((session, _, pane)) = view.looked()
                && self.joined(&session.id)
                && self.link.blocks.contains_key(&pane.id)
            {
                let apart = (session.id.clone(), false, false, pane.owner);
                kept.entry(pane.id.clone()).or_insert(apart).1 = true;
            }
            let selected = view.selected_session.clone();
            state.terminals.retain(|id, _| kept.contains_key(id));
            for (id, terminal) in &state.terminals {
                terminal.show(kept[id].1);
            }
            state.last.retain(|id, _| kept.contains_key(id));
            (selected, shown, kept)
        };
        // The panes of a session that is on show no longer are given up, as
        // leaving it would give them up: another client that wants them has
        // them at its own size, though this one stays to keep its tabs fed.
        if self.held != selected
            && let Some(left) = self.held.take()
            && self.joined(&left)
        {
            let request = json!({"session_id": left});
            let _ = self.rpc.enqueue("session.disown_block", request).await;
        }
        if let Some((session, _)) = &shown {
            self.held = Some(session.clone());
        }
        self.showing.retain(|pane| kept.contains_key(pane));
        self.streams.retain(|id, (task, _)| {
            let keep = kept.contains_key(id);
            if !keep {
                task.abort();
            }
            keep
        });
        // Those that have just come on show and are another client's.
        let mut others = Vec::new();
        for (pane, (session, drawn, on_show, owner)) in kept {
            match self.streams.get(&pane) {
                Some((_, shown)) => shown.store(drawn, Ordering::Relaxed),
                None => {
                    // Its colours are this client's while the terminal is: the
                    // server takes them up whenever this client comes to own it.
                    if let Some(theme) = &self.link.theme {
                        let request =
                            json!({"session_id": session, "block_id": pane, "args": theme});
                        let _ = self.rpc.enqueue(THEME, request).await;
                    }
                    let shown = Arc::new(AtomicBool::new(drawn));
                    let stream = stream_pane(
                        self.target.clone(),
                        self.rpc.clone(),
                        self.shared.clone(),
                        session,
                        pane.clone(),
                        shown.clone(),
                    );
                    self.streams.insert(pane.clone(), (tokio::spawn(stream), shown));
                }
            }
            if !on_show {
                self.showing.remove(&pane);
            } else if self.showing.insert(pane.clone()) && !owner {
                others.push(pane);
            }
        }
        // They are taken, to be the size this window gives them: the window
        // looked at is the one to fit. Nothing waits on the answer.
        if let Some((session, _)) = shown.filter(|_| !others.is_empty()) {
            let request = json!({"session_id": session, "block_ids": others, "mode": "take"});
            let _ = self.rpc.enqueue("session.own_block", request).await;
        }
    }

    /// Change what is known of a terminal, and show it. What its programs newly
    /// have to say by that is passed on if `loud`.
    fn note(&mut self, id: &str, loud: bool, change: impl FnOnce(&mut Block)) {
        let Some(block) = self.link.blocks.get_mut(id) else {
            return;
        };
        let before = block.says().0;
        change(block);
        let said = block.news(before).filter(|_| loud);
        self.shared.change(|s| fill(&mut s.view.sessions, &self.link.blocks));
        if let Some(said) = said {
            self.tell(id, &said);
        }
    }

    /// Pass on what a pane's programs have to say, if anything, for whoever is not
    /// looking at it.
    fn tell(&self, pane: &str, said: &str) {
        let state = self.shared.lock();
        let held = state.view.find(pane).filter(|_| !said.is_empty());
        if let Some((session, tab, _)) = held {
            let herald = &self.shared.herald;
            herald.say(pane, &tab.id, tab.name(), &session.name, said);
            // A window that is looking at the pane draws, and so says that it is.
            self.shared.wake.notify();
        }
    }

    /// A terminal's bell has rung. One that rings and rings is heard as one.
    fn rang(&self, pane: String) {
        self.tell(&pane, Activity::Rang.word().unwrap_or_default());
        self.shared.change(|s| {
            s.view.rang.insert(pane);
        });
    }

    /// Hand the UI the text a program has copied, if its pane is on show. Text too
    /// long for the server to have sent with the news is asked for apart from all
    /// else that goes on here, which does not wait for it. A later copy takes the
    /// place of one still being asked for.
    fn copied(&mut self, pane: String, text: Option<String>, id: u64) {
        let on_show =
            |(_, shown): &(JoinHandle<()>, Arc<AtomicBool>)| shown.load(Ordering::Relaxed);
        let on_show = self.streams.get(&pane).is_some_and(on_show);
        let Some(block) = self.link.blocks.get(&pane).filter(|_| on_show) else {
            return;
        };
        let (rpc, shared, session) = (self.rpc.clone(), self.shared.clone(), block.session.clone());
        let fetch = tokio::spawn(async move {
            let text = match text {
                Some(text) => Ok(text),
                None => {
                    whole(&rpc, &session, &pane, COPIED, id, MAX_COPY_BYTES, String::new()).await
                }
            };
            match text {
                Ok(text) => shared.change(|s| s.view.copied = Some((pane, text))),
                Err(error) => shared.fail(format!("Nothing was copied: {error:#}")),
            }
        });
        if let Some(older) = self.copying.replace(fetch) {
            older.abort();
        }
    }

    /// Take a pane on show for this client, unless it has it, so that the pane is
    /// the size this window gives it. The asking goes ahead of the input it is for,
    /// and the server takes the two in that order. It is made again until a view
    /// says the pane is taken, which does no harm. Only one who has joined the
    /// session may ask.
    async fn claim(&mut self, session: &str, pane: &str) {
        let joined = self.joined(session);
        let another = |held: &Pane| joined && !held.owner;
        if self.shared.lock().view.pane(pane).is_some_and(another) {
            let request = json!({"session_id": session, "block_ids": [pane], "mode": "take"});
            self.ask(Reply::Told("Input failed"), "session.own_block", request).await;
        }
    }

    /// What takes the keyboard to a pane, for `perform` to ask. The pane is
    /// taken for this client first, and its layer comes to the front where
    /// another floats in front of it, which the server leaves to be asked.
    /// One `brought` out from out of sight has the window's keyboard when it
    /// is seen: focus ends a zoom that hides it, and brings back a floating
    /// layer that was put away.
    async fn focus(
        &mut self,
        pane: String,
        brought: bool,
    ) -> Result<(String, &'static str, Value, Option<String>)> {
        let session = self.session(&pane)?;
        self.claim(&session, &pane).await;
        let covered = {
            let state = self.shared.lock();
            let found = state.view.find(&pane);
            found.and_then(|(_, tab, pane)| tab.covered(pane)).map(|layer| layer.id.clone())
        };
        if let Some(layer) = covered {
            let request = json!({"session_id": session, "layer_id": layer});
            let _ = self.rpc.enqueue("session.raise_layer", request).await;
        }
        let payload = json!({"block_id": pane});
        Ok((session, "session.focus_block", payload, brought.then_some(pane)))
    }

    /// See whether the server has moved its tab from what was `shown` when it
    /// was asked to act for the user, if anything was.
    async fn moved(&mut self, shown: Option<(String, String)>) {
        if let Some((session, tab)) = shown {
            let request = json!({"session_id": session});
            self.ask(Reply::Moved(session, tab), "session.view", request).await;
        }
    }

    /// Have the server show the tab this window shows, where it shows another,
    /// before it is asked to act for the user: what its configuration does, it
    /// does to the tab the server shows. Returns the session and the tab on show.
    async fn agree(&mut self) -> Option<(String, String)> {
        let (session, tab, agreed) = {
            let state = self.shared.lock();
            let (session, tab) = (state.view.session()?, state.view.selected_tab.clone()?);
            (session.id.clone(), tab.clone(), session.active.as_ref() == Some(&tab))
        };
        if !agreed && self.joined(&session) {
            let request = json!({"session_id": session, "window_id": tab});
            let reply = Reply::Change(session.clone(), None, false);
            self.ask(reply, "session.focus_window", request).await;
        }
        Some((session, tab))
    }

    /// A new shell, as a layout asks for one. It starts in the directory that
    /// the pane it is made `from` is in, where the server knows that, and in
    /// this client's colours: it need not start in others and then change.
    fn shell(&self, from: Option<&str>) -> Value {
        let mut options = json!({});
        let block = from.and_then(|pane| self.link.blocks.get(pane));
        if let Some(folder) = block.and_then(|block| block.running.folder.as_ref()) {
            options["cwd"] = json!(folder);
        }
        if let Some(theme) = &self.link.theme {
            options["theme"] = theme.clone();
        }
        json!({"block": {"flavor": "com.superlogical.terminal.shell", "options": options}})
    }

    /// Give the terminals this client feeds these colours, and those it comes to
    /// feed. None leaves each the ones it has.
    async fn retheme(&mut self, theme: Option<Value>) {
        self.link.theme = theme.clone();
        let Some(theme) = theme else { return };
        let fed: Vec<String> = self.streams.keys().cloned().collect();
        for pane in fed {
            let Ok(session) = self.session(&pane) else { continue };
            let request = json!({"session_id": session, "block_id": pane, "args": theme});
            let _ = self.rpc.enqueue(THEME, request).await;
        }
    }

    /// The session that holds a pane, which is the one to name in asking anything
    /// of the pane, whatever has come on show since.
    fn session(&self, pane: &str) -> Result<String> {
        let state = self.shared.lock();
        let (session, ..) = state.view.find(pane).context("That pane has closed")?;
        Ok(session.id.clone())
    }

    /// The pane that speaks for a tab, in whatever session the tab is.
    fn front(&self, tab: &str) -> Option<String> {
        let state = self.shared.lock();
        let (_, tab) = state.view.every_tab().find(|(_, held)| held.id == tab)?;
        tab.front().map(|pane| pane.id.clone())
    }

    /// Do what the UI asked, which is to ask it of the server and go on: typing
    /// never waits on a round trip.
    async fn perform(&mut self, action: Action) -> Result<()> {
        // A change to a pane names the pane, and its session is found from that.
        let (session, method, mut payload, made) = match action {
            // What is on show is as asked already, and `show` joins or leaves.
            Action::SelectSession(_) | Action::Shown => {
                self.show().await;
                return Ok(());
            }
            Action::CreateSession(name) => {
                let mut payload = json!({"initial_windows": [{"layout": self.shell(None)}]});
                if let Some(name) = name {
                    payload["label"] = json!(name);
                }
                (String::new(), "session.create", payload, None)
            }
            Action::SelectTab { session, tab } => {
                let payload = json!({"window_id": tab});
                (session, "session.focus_window", payload, None)
            }
            Action::CreateTab(session) => {
                // It starts where the pane with the keyboard is, in the tab on
                // show: asking for a tab has put its session on show already.
                let from = {
                    let view = &self.shared.lock().view;
                    let here = view.selected_session.as_ref() == Some(&session);
                    let shown = view.current_tab().filter(|_| here);
                    shown.and_then(Tab::front).map(|pane| pane.id.clone())
                };
                let payload = json!({"layout": self.shell(from.as_deref()), "focus": true});
                (session, "session.new_window", payload, None)
            }
            Action::Edit { session, tab, edit } => {
                let (method, mut payload) = match (edit, tab.is_some()) {
                    (Edit::Rename(name), true) => {
                        ("session.set_window_label", json!({"label": name}))
                    }
                    (Edit::Rename(name), false) => ("session.set_label", json!({"label": name})),
                    (Edit::MoveBefore(before), true) => {
                        ("session.move_window", json!({"before_window_id": before}))
                    }
                    (Edit::MoveBefore(before), false) => {
                        ("session.move", json!({"before_session_id": before}))
                    }
                    (Edit::Close, true) => ("session.close_window", json!({})),
                    (Edit::Close, false) => ("session.destroy", json!({})),
                };
                if let Some(tab) = tab {
                    payload["window_id"] = json!(tab);
                }
                (session, method, payload, None)
            }
            Action::Split { pane, toward: [x, y] } => {
                let payload = json!({
                    "anchor_id": pane,
                    "direction": if y == 0.0 { "horizontal" } else { "vertical" },
                    "side": if x + y < 0.0 { "before" } else { "after" },
                    "layout": self.shell(Some(&pane)),
                    "focus": true,
                });
                (self.session(&pane)?, "session.new_split", payload, None)
            }
            Action::MovePane { pane, tab: Some(tab) } => {
                let session = self.session(&pane)?;
                let front = self.front(&tab).context("That tab has closed")?;
                let payload = json!({"block_id": pane, "anchor_block_id": front});
                (session, "session.move_block", payload, Some(pane))
            }
            Action::MovePane { pane, tab: None } => {
                let session = self.session(&pane)?;
                let request =
                    json!({"session_id": session, "layout": self.shell(None), "focus": false});
                self.ask(Reply::Room(session, pane), "session.new_window", request).await;
                return Ok(());
            }
            Action::Swap { pane, with } => {
                let payload = json!({"block_id": pane, "other_block_id": with});
                (self.session(&pane)?, "session.swap_blocks", payload, None)
            }
            Action::Divide(borders) => {
                let request = |border: &Division| {
                    let request = json!({
                        "session_id": self.session(&border.before)?,
                        "direction": if border.upright { "horizontal" } else { "vertical" },
                        "before_block_id": border.before,
                        "after_block_id": border.after,
                        "ratio": border.ratio,
                    });
                    anyhow::Ok(("session.resize_pane", request))
                };
                let requests = borders.iter().map(request).collect::<Result<_>>()?;
                let _ = self.shaping.send(requests);
                return Ok(());
            }
            Action::NewFloat { session, tab, bounds } => {
                // It starts where the pane with the keyboard is, as a new tab does.
                let layout = self.shell(self.front(&tab).as_deref());
                let payload =
                    json!({"window_id": tab, "bounds": bounds, "layout": layout, "focus": true});
                (session, "session.new_layer", payload, None)
            }
            Action::Float { pane, bounds } => {
                let payload = json!({"block_id": pane, "bounds": bounds});
                (self.session(&pane)?, "session.float_block", payload, None)
            }
            Action::Dock { pane, beside } => {
                let payload = json!({"block_id": pane, "anchor_block_id": beside});
                (self.session(&pane)?, "session.move_block", payload, None)
            }
            Action::Place { session, layer, bounds } => {
                let request = json!({"session_id": session, "layer_id": layer, "bounds": bounds});
                let _ = self.shaping.send(vec![("session.resize_layer", request)]);
                return Ok(());
            }
            Action::Stow { session, layers, back } => {
                let state = if back.is_some() { "normal" } else { "minimized" };
                for layer in layers {
                    let request = json!({"session_id": session, "layer_id": layer, "state": state});
                    let kept = Reply::Told("The floating panes are as they were");
                    self.ask(kept, "session.set_layer_state", request).await;
                }
                // They are brought back for a pane, which then has the keyboard.
                let Some(pane) = back else {
                    return Ok(());
                };
                self.focus(pane, true).await?
            }
            Action::Zoom { pane } => {
                let session = self.session(&pane)?;
                // Only an owner may resize it to the room it is given.
                self.claim(&session, &pane).await;
                let payload = json!({"block_id": pane});
                (session, "session.zoom_block", payload, None)
            }
            Action::ClosePane { pane } => {
                let payload = json!({"block_id": pane});
                (self.session(&pane)?, "block.close", payload, None)
            }
            Action::Reveal { pane } => self.focus(pane, true).await?,
            Action::FocusPane { pane } => self.focus(pane, false).await?,
            Action::Perform { name, args, origin, pane, servers, far } => {
                // A terminal's action is done to a pane, in the pane's session.
                // One of the configuration's is given what there is: which is
                // another server's, where the window shows that one.
                let (server, session) = match (far, &pane) {
                    (Some((server, session)), _) => (Some(server), Some(session)),
                    (None, Some(pane)) => (None, Some(self.session(pane)?)),
                    (None, None) => (None, self.shared.lock().view.selected_session.clone()),
                };
                let named = |name: &String| json!({"name": name, "id": name});
                let listed = || servers.iter().map(named).collect::<Vec<_>>();
                let servers = (!servers.is_empty()).then(listed);
                let mut payload = json!({"action": name, "origin": origin});
                let fields = [
                    ("args", args),
                    ("session_id", json!(session)),
                    ("block_id", json!(pane)),
                    ("servers", json!(servers)),
                    ("server", json!(server)),
                ];
                for (key, value) in fields {
                    if !value.is_null() {
                        payload[key] = value;
                    }
                }
                // What it does to another server's tab, it does through a call
                // made there, which sees to the tab then.
                let shown = match server {
                    Some(_) => None,
                    None => self.agree().await,
                };
                self.ask(Reply::Performed(shown), "client.action", payload).await;
                return Ok(());
            }
            Action::Call(call) => {
                let shown = self.agree().await;
                self.ask(Reply::Relayed(call.call, shown), &call.method, call.args).await;
                return Ok(());
            }
            Action::Called(model::Called { call, answer }) => {
                // The action has stopped waiting if this comes too late.
                let _ = self.rpc.enqueue("client.reply", reply(&call, answer)).await;
                return Ok(());
            }
            Action::Reply { call, error } => {
                // Whoever asked has stopped waiting if this comes too late, and
                // there is nobody to tell.
                let _ = self.rpc.enqueue("client.reply", reply(&call, outcome(error))).await;
                return Ok(());
            }
            Action::Answer { call, answer } => {
                let result = match answer {
                    model::Answer::Text(text) => json!({"text": text}),
                    model::Answer::Chosen(ids) => json!({"ids": ids}),
                    model::Answer::Refused => json!({"cancelled": true}),
                };
                let payload = reply(&call, Ok(result));
                self.ask(Reply::Told("The answer was not taken"), "client.reply", payload).await;
                return Ok(());
            }
            Action::Upload { pane, files } => {
                let session = self.session(&pane)?;
                // Files on the pane's own machine are named where they are.
                let here = files.iter().map(|file| file.path().filter(|_| self.link.local));
                if let Some(here) = here.collect::<Option<Vec<_>>>() {
                    let quoted = here.iter().map(|path| upload::quoted(&path.to_string_lossy()));
                    let text = quoted.collect::<Vec<_>>().join(" ");
                    return self.input(Action::Paste { pane, text }).await;
                }
                let (rpc, target, shared) =
                    (self.rpc.clone(), self.target.clone(), self.shared.clone());
                tokio::spawn(async move {
                    let sent = upload::send(&rpc, &target, &session, &pane, &files).await;
                    if let Err(error) = sent {
                        shared.fail(format!("Nothing was sent to the pane: {error:#}"));
                    }
                });
                return Ok(());
            }
            // As its lines were written, not as the pane's width broke them. It
            // is asked for apart from all else that goes on here, as it may be long.
            Action::CopyAll { pane } => {
                let session = self.session(&pane)?;
                let (rpc, shared) = (self.rpc.clone(), self.shared.clone());
                tokio::spawn(async move {
                    let args = json!({"trim": true, "unwrap": true, "max_bytes": COPY_PART_BYTES});
                    let request = json!({"session_id": session, "block_id": pane, "args": args});
                    let text = async {
                        let first = rpc.call(FORMAT, request).await?;
                        let text = first["content"].as_str().unwrap_or_default().to_owned();
                        // What did not all come at once is kept, for the rest to be read.
                        match first["id"].as_u64() {
                            Some(id) => {
                                whole(&rpc, &session, &pane, BLOB, id, MAX_OUTPUT_BYTES, text).await
                            }
                            None => Ok(text),
                        }
                    };
                    match text.await {
                        Ok(text) => shared.change(|s| s.view.output = Some(text)),
                        Err(error) => shared.fail(format!("Nothing was copied: {error:#}")),
                    }
                });
                return Ok(());
            }
            // Every client is told of a list that has changed, this one too,
            // and reads it again then.
            Action::PutHost(host) => {
                let mut put = json!({"label": host.label, "endpoints": host.endpoints});
                if !host.id.is_empty() {
                    put["id"] = json!(host.id);
                }
                self.ask(Reply::Told(HOSTS_KEPT), "hosts.put", json!({"host": put})).await;
                return Ok(());
            }
            Action::RemoveHost(id) => {
                self.ask(Reply::Told(HOSTS_KEPT), "hosts.remove", json!({"id": id})).await;
                return Ok(());
            }
            input => return self.input(input).await,
        };
        // A session yet to be made has no name to go by.
        if !session.is_empty() {
            payload["session_id"] = json!(session);
        }
        let new = matches!(method, "session.create" | "session.new_window");
        let reply = Reply::Change(session, made, new);
        self.ask(reply, method, payload).await;
        self.show().await;
        Ok(())
    }

    /// Queue input for a terminal, in the order it came.
    async fn input(&mut self, action: Action) -> Result<()> {
        let event =
            |kind: &str, payload: Value| json!({"event": {"type": kind, "payload": payload}});
        // What is typed, which a terminal takes before it has drawn.
        let typed =
            matches!(action, Action::Key { .. } | Action::Text { .. } | Action::Paste { .. });
        // A key that a function on the server answers is the server's to act on.
        let served = matches!(action, Action::Key { served: Some(_), .. });
        // Input that engages the terminal takes ownership and returns to live
        // output. A size engages nothing, and is taken by a terminal not yet seen.
        let (pane, method, mut payload, engage) = match action {
            Action::Resize { pane, size } => {
                ensure!(size.cols > 0 && size.rows > 0, "Terminal dimensions must be nonzero");
                // The server holds the size asked by a client that does not own the
                // terminal, for when it does.
                let args = json!({
                    "columns": size.cols,
                    "rows": size.rows,
                    "cell_width_px": size.cell_width,
                    "cell_height_px": size.cell_height,
                });
                let method = "com.superlogical.terminal.resize";
                (pane, method, json!({"args": args}), None)
            }
            Action::Key { pane, key, code, modifiers, pressed, repeat, served } => {
                let stroke = match (pressed, repeat) {
                    (false, _) => "release",
                    (true, false) => "press",
                    (true, true) => "repeat",
                };
                let mut payload = json!({"key": key_payload(&key, &code, modifiers, stroke)});
                if let Some(prefix) = served.filter(|prefix| !prefix.is_empty()) {
                    payload["prefix"] = json!(prefix);
                }
                // A modifier held alone asks nothing of the terminal.
                let engage = pressed && !model::is_modifier(&code);
                (pane, "client.key", payload, Some(engage))
            }
            Action::Text { pane, text } => {
                let payload = json!({"key": key_payload(&text, "unidentified", 0, "press")});
                (pane, "client.key", payload, Some(true))
            }
            Action::Paste { pane, text } => {
                let payload = event("paste", json!({"text": text}));
                (pane, "block.event", payload, Some(true))
            }
            Action::Mouse { pane, kind, x, y, button, modifiers } => {
                // Shift is the local override for mouse capture; programs never see it.
                let mods = modifier_names(modifiers & !model::SHIFT);
                let mut mouse = json!({"action": kind, "x": x, "y": y, "mods": mods});
                if let Some(button) = button {
                    mouse["button"] = json!(button);
                }
                (pane, "block.event", event("mouse", mouse), Some(kind == "press"))
            }
            Action::Wheel { pane, x, y, lines } => {
                let wheel = json!({"x": x, "y": y, "delta_x": 0, "delta_y": lines});
                (pane, "block.event", event("wheel", wheel), Some(false))
            }
            // The server passes it on only to a program that asked to be told.
            Action::Focus { pane, gained } => {
                let focus = json!({"action": if gained { "in" } else { "out" }});
                (pane, "block.event", event("focus", focus), Some(false))
            }
            // Done by `perform`, or by the connection loop.
            _ => return Ok(()),
        };
        // The session is the terminal's own, whatever has come on show since the
        // input was made. What is no terminal, or is gone, takes none.
        let Some(block) = self.link.blocks.get(&pane) else {
            return Ok(());
        };
        let session = block.session.clone();
        payload["session_id"] = json!(session);
        payload["block_id"] = json!(pane);
        if let Some(engage) = engage {
            let terminal = self.shared.terminal(&pane);
            // A terminal that is fed no more takes nothing: what it shows, if
            // anything, is what it last showed. One that is being fed and has yet
            // to draw, as a pane just made is, takes what is typed ahead: only the
            // pointer needs a screen to mean anything.
            let lost = match &terminal {
                Some(terminal) => terminal.status().is_some(),
                None => self.shared.lock().last.contains_key(&pane),
            };
            let drawn = terminal.as_ref().is_some_and(|t| t.latest().is_some());
            if lost || !(typed || drawn) {
                // A release or a motion may trail a pane that has left the screen.
                // Only input that would have done something is worth a complaint.
                ensure!(!engage, "Terminal is not ready; input was not sent");
                return Ok(());
            }
            if engage {
                self.claim(&session, &pane).await;
                if let Some(terminal) = terminal {
                    terminal.ask(Request::Scroll(Scroll::Bottom));
                }
            }
        }
        let reply = match served {
            true => Reply::Performed(self.agree().await),
            false => Reply::Told("Input failed"),
        };
        self.ask(reply, method, payload).await;
        Ok(())
    }
}

/// The answer to something the server asked of this client by a `call`: what
/// came of it, or the code of an error and the words for it.
fn reply(call: &str, answer: Result<Value, [String; 2]>) -> Value {
    match answer {
        Ok(result) => json!({"call_id": call, "result": result}),
        Err([code, message]) => {
            json!({"call_id": call, "error": {"code": code, "message": message}})
        }
    }
}

/// What came of another client's asking: that what it asked was done, or by
/// the code of an error that it was not, with the words for that.
fn outcome(error: Option<&str>) -> Result<Value, [String; 2]> {
    let Some(code) = error else {
        return Ok(Value::Null);
    };
    let message = match code {
        "refused" => "Rexy cannot do that just now",
        "remote_control_off" => {
            "Remote Control is off in this Rexy window. To allow it, choose \
             Allow Remote Control in the window's command palette."
        }
        _ => "Rexy has no such action",
    };
    Err([code.into(), message.into()])
}

/// The whole of a long text that a terminal keeps by an `id`, such as one a
/// program has copied, as `method` reads it: asked for a part at a time, after
/// the `text` of it that has come already. Longer than the `most` it may be,
/// it fails.
async fn whole(
    rpc: &Rpc,
    session: &str,
    pane: &str,
    method: &str,
    id: u64,
    most: usize,
    mut text: String,
) -> Result<String> {
    loop {
        let args = json!({"id": id, "offset": text.len(), "max_bytes": COPY_PART_BYTES});
        let request = json!({"session_id": session, "block_id": pane, "args": args});
        let part = rpc.call(method, request).await?;
        let more = part["data"].as_str().unwrap_or_default();
        text.push_str(more);
        ensure!(text.len() <= most, "it is too long");
        if part["eof"] == true {
            return Ok(text);
        }
        ensure!(!more.is_empty(), "the server sent only part of it");
    }
}

/// Keep one pane's terminal fed for as long as its tab is kept so, restoring a
/// fresh snapshot whenever its stream fails: soon at first, and seldom if it
/// keeps failing. Only while it is `shown` does it draw, and wake the window.
async fn stream_pane(
    target: wire::Target,
    rpc: Rpc,
    shared: Shared,
    session: String,
    pane: String,
    shown: Arc<AtomicBool>,
) {
    let mut failures = 0;
    loop {
        let began = std::time::Instant::now();
        let Err(error) = stream(&target, &rpc, &shared, &session, &pane, &shown).await;
        // One that lasted was working. One that draws and fails, over and over, is not.
        if began.elapsed() > RETRY[1] {
            failures = 0;
        }
        let wait = retry(&error, failures, false);
        failures += 1;
        shared.change(|s| {
            s.rest(&pane);
            if let Some(pane) = s.view.pane_mut(&pane) {
                pane.status = match wait {
                    Some(_) => format!("Reconnecting terminal: {error:#}"),
                    None => format!("{error:#}"),
                };
            }
        });
        // A terminal that has closed has nothing to restore, and the next view
        // is without its pane.
        let Some(wait) = wait else {
            return;
        };
        tokio::time::sleep(wait).await;
    }
}

async fn stream(
    target: &wire::Target,
    rpc: &Rpc,
    shared: &Shared,
    session: &str,
    pane: &str,
    shown: &Arc<AtomicBool>,
) -> Result<Infallible> {
    let request = json!({
        "session_id": session,
        "block_id": pane,
        "data_ref_id": "terminal",
        "subprotocol": wire::DATA_PROTOCOL,
    });
    let response = rpc.call("data.connect", request).await?;
    let ticket = response["ticket"].as_str().context("Missing terminal ticket")?;
    let mut socket = wire::socket(target, "/api/data", wire::DATA_PROTOCOL).await?;
    let nonce = wire::handshake(&mut socket, ticket).await?;
    // The worker says when it has drawn, and so when it has stopped. One that
    // stops while its terminal is quiet would otherwise go unnoticed here, and
    // its pane take no input, until the next output came.
    let (wake, drawn) = (shared.wake.clone(), Arc::new(tokio::sync::Notify::new()));
    let (said, seen) = (drawn.clone(), shown.clone());
    let repaint = move || {
        if seen.load(Ordering::Relaxed) {
            wake.notify();
        }
        said.notify_one();
    };
    let terminal = TerminalHandle::new(nonce, Arc::new(repaint), shown.load(Ordering::Relaxed))?;
    shared.change(|s| {
        s.terminals.insert(pane.into(), terminal.clone());
        s.last.remove(pane);
    });
    loop {
        tokio::select! {
            bytes = wire::next_binary(&mut socket) => terminal.feed(bytes?).await?,
            _ = drawn.notified() => {
                if let Some(error) = terminal.status() {
                    bail!(error);
                }
            }
        }
    }
}

fn items(value: &Value) -> impl Iterator<Item = &Value> + Clone {
    value.as_array().into_iter().flatten()
}

fn label(value: Option<&str>, fallback: &str) -> String {
    value.filter(|s| !s.is_empty()).unwrap_or(fallback).into()
}

/// The entry for one block in a server block list, or null.
fn block<'a>(blocks: &'a Value, id: &str) -> &'a Value {
    items(blocks).find(|b| b["block_id"] == id).unwrap_or(&NULL)
}

/// Bring the list of sessions up to date with news of it. Told the same thing
/// twice, it is as it was when told once.
fn list(sessions: &mut Vec<Session>, event: &Event) {
    let at = |sessions: &[Session], id: &str| sessions.iter().position(|s| s.id == id);
    match event {
        Event::Created { session, name } if at(sessions, session).is_none() => {
            sessions.push(Session {
                id: session.clone(),
                name: label(Some(name), "Untitled"),
                ..Default::default()
            });
        }
        Event::Destroyed { session } => sessions.retain(|held| &held.id != session),
        Event::Moved { session, before } => {
            let Some(from) = at(sessions, session) else {
                return;
            };
            let moved = sessions.remove(from);
            let to = before.as_ref().and_then(|before| at(sessions, before));
            sessions.insert(to.unwrap_or(sessions.len()), moved);
        }
        _ => {}
    }
}

/// Move borders and floating layers as they are asked to be moved, one request
/// at a time, each by its method: a drag asks faster than a server answers,
/// and every move reflows the terminals it touches. Only where they were last
/// asked to go is asked of the server, once it has answered for the time
/// before. The view that comes of it is in the server's news.
fn shape(rpc: Rpc, shared: Shared) -> watch::Sender<Vec<(&'static str, Value)>> {
    let (shaping, mut asked) = watch::channel(Vec::new());
    tokio::spawn(async move {
        // Until the connection is dropped, and the asking with it.
        while asked.changed().await.is_ok() {
            let requests = asked.borrow_and_update().clone();
            for (method, request) in requests {
                // What the server would not do is said. That the connection ended
                // is said already, above its last screen.
                if let Err(error) = rpc.call(method, request).await
                    && error.is::<Failed>()
                {
                    shared.fail(format!("{error:#}"));
                }
            }
        }
    });
    shaping
}

/// The panes under one node of a layout, each with the place the server gives it:
/// a fraction of the tab. With them, the border of every split under it. A layout
/// is no deeper than the JSON it came in, which the parser keeps to 128 levels.
fn layout(node: &Value, panes: &mut Vec<(String, [f32; 4])>, borders: &mut Vec<Border>) {
    let Some(room) = rect(&node["rect"]) else {
        return;
    };
    if let Some(id) = node["block_id"].as_str() {
        return panes.push((id.into(), room));
    }
    let split = &node["split"];
    let before = panes.len();
    layout(&split["before"], panes, borders);
    let after = panes.len();
    layout(&split["after"], panes, borders);
    // The first pane of each side names the split, and the second side begins at
    // the border. A side with no pane has no border to it.
    if let (Some((first, _)), Some((second, place))) =
        (panes[..after].get(before), panes.get(after))
    {
        // A direction not known is drawn as this one.
        let upright = split["direction"] != "vertical";
        borders.push(Border {
            before: first.clone(),
            after: second.clone(),
            upright,
            room,
            at: place[usize::from(!upright)],
            float: None,
        });
    }
}

/// The tabs of a view. The panes of them that are terminals are named in `terminals`.
fn parse_tabs(view: &Value, client_id: &str, terminals: &mut Vec<String>) -> Vec<Tab> {
    items(&view["windows"])
        .filter_map(|tab| {
            let id: String = tab["window_id"].as_str()?.into();
            let zoom = tab["zoomed_block_id"].as_str();
            let (mut panes, mut hidden, mut borders) = (Vec::new(), Vec::new(), Vec::new());
            let mut floats = Vec::new();
            for layer in items(&tab["layers"]) {
                let (mut placed, mut split) = (Vec::new(), Vec::new());
                layout(&layer["layout"], &mut placed, &mut split);
                // One layer fills the tab, and the others float over it in the
                // order they come: a layer of no kind is taken to fill the tab.
                // Each has borders to drag while its panes are all on show.
                let floating = layer["kind"] == "floating";
                let float = layer["layer_id"].as_str().filter(|_| floating);
                let away = layer["state"] == "minimized";
                if zoom.is_none() && !away {
                    split.iter_mut().for_each(|border| border.float = float.map(str::to_owned));
                    borders.append(&mut split);
                }
                if let Some((id, bounds)) = float.zip(rect(&layer["bounds"])) {
                    floats.push(Float { id: id.into(), bounds, away });
                }
                for (id, bounds) in placed {
                    let block = block(&layer["blocks"], &id);
                    // What is not a terminal keeps its place, and says what it is.
                    let creator = block["creator_name"].as_str();
                    let other = creator.filter(|creator| *creator != TERMINAL);
                    if other.is_none() {
                        terminals.push(id.clone());
                    }
                    // A zoomed pane fills the tab, and the rest are out of sight behind
                    // it. So are those of a floating layer that has been put away.
                    let (panes, bounds) = match zoom {
                        _ if away => (&mut hidden, bounds),
                        Some(zoom) if zoom == id => (&mut panes, FULL),
                        Some(_) => (&mut hidden, bounds),
                        None => (&mut panes, bounds),
                    };
                    let [x, y, width, height] = bounds;
                    let named = label(block["label"].as_str(), other.unwrap_or("Terminal"));
                    panes.push(Pane {
                        id,
                        title: named.clone(),
                        label: named,
                        program: Program::default(),
                        activity: Activity::Idle,
                        message: String::new(),
                        task: String::new(),
                        progress: None,
                        x,
                        y,
                        width,
                        height,
                        frame: None,
                        status: match other {
                            Some(other) => format!("{other}: not a terminal"),
                            None => "Connecting terminal…".into(),
                        },
                        owner: block["owner"] == client_id,
                        idle: false,
                        float: float.map(str::to_owned),
                    });
                }
            }
            // A tab nobody named is labelled by the server with the end of its ID.
            let label = tab["label"].as_str().filter(|label| {
                let unnamed = label.strip_prefix("Window ");
                !label.is_empty() && !unnamed.is_some_and(|tail| id.ends_with(tail))
            });
            Some(Tab {
                label: label.map(str::to_owned),
                focus: tab["focused_block_id"].as_str().map(str::to_owned),
                zoomed: zoom.is_some(),
                id,
                panes,
                hidden,
                borders,
                floats,
            })
        })
        .collect()
}

/// What one record of a program's says it is doing.
fn state(record: &Value) -> Activity {
    match record["state"].as_str() {
        Some("working") => Activity::Working,
        Some("done") => Activity::Done,
        Some("error") => Activity::Error,
        Some("blocked") => Activity::Blocked(match record["kind"].as_str() {
            Some("permission") => Wait::Approval,
            Some("question") => Wait::Question,
            Some("auth") => Wait::SignIn,
            _ => Wait::Input,
        }),
        _ => Activity::Idle,
    }
}

/// What a terminal's programs say they are doing, of all that each has reported:
/// the most pressing of it, the record that says so, and the icon of one still
/// at it.
fn reported<'a>(
    records: impl Iterator<Item = &'a Value> + Clone,
) -> (Activity, &'a Value, Option<Program>) {
    // Of several as pressing, the first: a program comes before its parts.
    let records = || records.clone().map(|record| (state(record), record));
    let top = records().min_by_key(|(state, _)| Reverse(*state));
    // What a program that has finished left behind says nothing of what runs now.
    let live = records().filter(|(state, _)| !matches!(state, Activity::Done | Activity::Error));
    let app = Program::named(live.filter_map(|(_, record)| record["effective_app"].as_str()));
    let (top, record) = top.unwrap_or((Activity::Idle, &Value::Null));
    (top, record, app)
}

/// Give every pane what is known of its terminal.
fn fill(sessions: &mut [Session], blocks: &HashMap<String, Block>) {
    let tabs = sessions.iter_mut().flat_map(|s| &mut s.tabs);
    for pane in tabs.flat_map(Tab::every_mut) {
        if let Some(block) = blocks.get(&pane.id) {
            let running = &block.running;
            pane.idle = block.shell.is_some() && block.shell == running.pid;
            // At its shell's prompt it goes by the shell and where that is: the
            // title a prompt sets tells who is at what machine first, and at
            // length. A program that gives itself no title goes by its own name,
            // and a terminal of which nothing is known by its label.
            if let Some(folder) = running.folder.as_ref().filter(|_| pane.idle) {
                let folder = model::folder(folder, &block.home);
                format!("{} {folder}", block.prompt).trim_start().clone_into(&mut pane.title);
            } else if !block.title.is_empty() {
                pane.title.clone_from(&block.title);
            } else if !running.name.is_empty() {
                pane.title.clone_from(&running.name);
            } else {
                pane.title.clone_from(&pane.label);
            }
            // What it is doing, by the record that speaks for it: the line said of
            // that, what it is at, and how far along. Failing the last, how far the
            // terminal itself was told.
            let (activity, record, app) = reported(block.records.values());
            let said = |key: &str| record[key].as_str().unwrap_or_default();
            pane.program = app.or(running.program).unwrap_or_default();
            pane.activity = activity;
            said("msg").clone_into(&mut pane.message);
            said("title").clone_into(&mut pane.task);
            pane.progress = wire::percent(&record["progress"]).or(block.progress);
        }
    }
}

fn rect(value: &Value) -> Option<[f32; 4]> {
    let out: [f32; 4] = serde_json::from_value(value.clone()).ok()?;
    (out.iter().all(|x| x.is_finite()) && out[2] > 0. && out[3] > 0.).then_some(out)
}

fn modifier_names(bits: u8) -> String {
    [(model::SHIFT, "shift"), (model::CTRL, "ctrl"), (model::ALT, "alt"), (model::SUPER, "super")]
        .into_iter()
        .filter_map(|(bit, name)| (bits & bit != 0).then_some(name))
        .collect::<Vec<_>>()
        .join("+")
}

/// A key as the server is told of it. `key` is what it types, `code` the name of
/// the key itself, and `stroke` is `press`, `repeat` or `release`.
fn key_payload(key: &str, code: &str, bits: u8, stroke: &str) -> Value {
    // Releases and Control/Super chords do not type anything.
    let text =
        if stroke != "release" && bits & (model::CTRL | model::SUPER) == 0 { key } else { "" };
    let shift = bits & model::SHIFT != 0;
    // Shift is used up in typing a capital or a symbol. In Shift+Tab it is not, nor
    // in Shift+Enter or Shift+Space: those type what they would without it.
    let shifted = shift && text.chars().any(|c| !c.is_control() && c != ' ');
    json!({
        "key": code,
        "mods": modifier_names(bits),
        "consumed_mods": if shifted { "shift" } else { "" },
        "text": text,
        "unshifted_codepoint": unshifted(key, code, shift),
        "composing": false,
        "action": stroke,
    })
}

/// The character a key types without Shift, or 0. What it typed says, unless Shift
/// was held. Then a letter is itself in small, and any other key is taken for the
/// one of a US keyboard, which is all that is known of it.
fn unshifted(key: &str, code: &str, shift: bool) -> u32 {
    let mut chars = key.chars();
    let typed = match (chars.next(), chars.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    };
    let legend = || {
        Some(match code {
            "space" => ' ',
            "tab" => '\t',
            "enter" => '\n',
            "backquote" => '`',
            "backslash" | "intl_backslash" => '\\',
            "bracket_left" => '[',
            "bracket_right" => ']',
            "comma" => ',',
            "equal" => '=',
            "minus" => '-',
            "period" => '.',
            "quote" => '\'',
            "semicolon" => ';',
            "slash" => '/',
            _ => {
                let name = code.strip_prefix("digit_");
                let mut name = name.or_else(|| code.strip_prefix("key_"))?.chars();
                name.next().filter(|_| name.next().is_none())?
            }
        })
    };
    let small = |c: char| c.to_lowercase().next().unwrap_or(c);
    let unshifted = match typed {
        Some(typed) if !shift || typed.is_alphabetic() => Some(small(typed)),
        _ => legend().or(typed),
    };
    unshifted.map_or(0, u32::from)
}

/// The connectivity probe that `--probe` runs. It makes no session and sends no
/// input, but it is a client like any other: it joins the first session, as a
/// window does, and asks there for the panes that no other client has.
pub fn probe(endpoint: String, timeout: Duration) -> Result<String> {
    let theme = watch::channel(None).1;
    let client = Client::spawn(vec![endpoint], Arc::new(|| {}), Herald::new().0, None, theme);
    let started = std::time::Instant::now();
    loop {
        let view = client.snapshot();
        // As a window does of the server it shows.
        if let Some(first) = view.unshown() {
            let _ = client.try_send(first);
            continue;
        }
        let frames = view.panes().filter(|p| p.frame.is_some()).count();
        if view.connected && (view.tabs().is_empty() || frames > 0) {
            return Ok(format!(
                "Connected; {} sessions, {} tabs, {} decoded terminal frames",
                view.sessions.len(),
                view.tabs().len(),
                frames
            ));
        }
        let pane_statuses = view
            .panes()
            .map(|p| p.status.as_str())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("; ");
        ensure!(
            started.elapsed() < timeout,
            "Probe timed out: {} · {}",
            view.status,
            pane_statuses
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

#[cfg(test)]
#[path = "../tests/unit/backend.rs"]
mod tests;
