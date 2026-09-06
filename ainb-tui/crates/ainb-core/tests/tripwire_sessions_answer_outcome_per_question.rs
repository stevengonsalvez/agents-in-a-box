//! Tripwire: a send's outcome belongs to the QUESTION it answered, and is
//! still there when the operator comes back to it.
//!
//! Two waiting questions. The first is raised on a worktree that TWO live
//! sessions share, and `resolve_and_send_typed` refuses an ambiguous target
//! rather than typing an answer into whichever pane the name happens to hit —
//! so the answer fails for real, through the real refusal, not a stub.
//!
//! A session whose tmux target is gone is not a usable fixture here: discovery
//! filters on `is_running`, so it never reaches the list to be answered.
//!
//! The regression this pins: the `ask` pane held ONE outcome field, cleared on
//! every retarget. Answering the first question and moving to the second threw
//! the failure away, so coming back to the question that failed showed a clean
//! composer and no reason — the answer had simply evaporated. Worse, an outcome
//! that landed while the pane had moved was written to whatever question was on
//! screen, so one session's failure was reported under another's.
//!
//! Driven through the real TUI in tmux and read back off the pane, because
//! that is the only place the lie was visible: every unit of this was green
//! while the surface was reporting the wrong question.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use ainb_plugin_notifyd::{Envelope, Paths, RetentionPolicy, Store};
use serde_json::json;

fn ainb_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ainb"))
}

fn tmux_available() -> bool {
    Command::new("tmux")
        .arg("-V")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A tmux session killed by EXACT name on drop.
struct ExactTmuxSession {
    name: String,
}

impl ExactTmuxSession {
    fn create(name: String, command: &[&str]) -> Self {
        let mut args: Vec<String> =
            ["new-session", "-d", "-s"].iter().map(|a| (*a).to_string()).collect();
        args.push(name.clone());
        args.extend(["-x", "200", "-y", "50"].iter().map(|a| (*a).to_string()));
        args.extend(command.iter().map(|part| (*part).to_string()));
        let status = Command::new("tmux").args(&args).status().expect("tmux new-session");
        assert!(status.success(), "tmux new-session {name} failed");
        Self { name }
    }
}

impl Drop for ExactTmuxSession {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args(["kill-session", "-t", &format!("={}", self.name)])
            .status();
    }
}

fn capture_pane(session: &str) -> String {
    let out = Command::new("tmux")
        .args(["capture-pane", "-t", session, "-p"])
        .output()
        .expect("tmux capture-pane");
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn send_key(session: &str, key: &str) {
    let status = Command::new("tmux")
        .args(["send-keys", "-t", session, key])
        .status()
        .expect("tmux send-keys");
    assert!(status.success(), "tmux send-keys {key:?} failed");
}

fn poll<F>(session: &str, deadline: Instant, mut ok: F) -> Option<String>
where
    F: FnMut(&str) -> bool,
{
    while Instant::now() < deadline {
        let cap = capture_pane(session);
        if ok(&cap) {
            return Some(cap);
        }
        thread::sleep(Duration::from_millis(400));
    }
    None
}

fn poll_until<F, G>(
    session: &str,
    key: &str,
    deadline: Instant,
    mut arrived: G,
    mut ok: F,
) -> Option<String>
where
    F: FnMut(&str) -> bool,
    G: FnMut(&str) -> bool,
{
    let mut on_screen = false;
    while Instant::now() < deadline {
        let cap = capture_pane(session);
        if ok(&cap) {
            return Some(cap);
        }
        on_screen = on_screen || arrived(&cap);
        if !on_screen {
            send_key(session, key);
        }
        thread::sleep(Duration::from_millis(400));
    }
    None
}

/// Press `key` until `ok` holds, re-checking between presses so a pane that
/// needs more than one repaint is not walked straight past.
fn press_until<F>(
    session: &str,
    key: &str,
    attempts: usize,
    mut ok: F,
) -> Result<String, Vec<String>>
where
    F: FnMut(&str) -> bool,
{
    let mut seen = Vec::new();
    for _ in 0..attempts {
        for _ in 0..4 {
            let cap = capture_pane(session);
            if ok(&cap) {
                return Ok(cap);
            }
            if seen.last() != Some(&cap) {
                seen.push(cap);
            }
            thread::sleep(Duration::from_millis(250));
        }
        send_key(session, key);
    }
    Err(seen)
}

/// Put `question`'s ask pane on screen, walking the session list a row at a
/// time and checking the pane after each step.
///
/// The question text lives ONLY in the ask pane, so walking the list while any
/// other tab is up matches nothing and runs straight off the end into the
/// host's own tmux sessions. The arrow is stolen by the ask pane, so each step
/// is Tab-off, arrow, Tab-back-around.
///
/// `arrow` because the list does not wrap into the sessions this test seeds:
/// below them sit the host's own tmux sessions, so the walk back is `Up`.
fn focus_question(session: &str, question: &str, arrow: &str, rows: usize) -> String {
    for step in 0..rows {
        if let Ok(pane) = press_until(session, "Tab", 6, |c| {
            c.contains("\u{2460} answer") && c.contains(question)
        }) {
            return pane;
        }
        if step + 1 < rows {
            // Off the ask tab so the arrows belong to the list again. Pressed
            // UNTIL it is off, not once: the strip has five tabs and the search
            // above leaves it wherever it ran out, so a single Tab can land
            // back ON `ask` — where `Down` moves the option cursor instead of
            // the session, and the walk never advances.
            press_until(session, "Tab", 6, |c| !c.contains("\u{2460} answer"))
                .expect("the strip has four tabs that are not `ask`");
            send_key(session, arrow);
            thread::sleep(Duration::from_millis(500));
        }
    }
    panic!(
        "never reached the ask pane for {question:?} in {rows} rows:\n{}",
        capture_pane(session)
    )
}

fn init_git_repo(dir: &Path) {
    let git = |args: &[&str]| {
        let status = Command::new("git")
            .current_dir(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "tripwire")
            .env("GIT_AUTHOR_EMAIL", "tripwire@example.invalid")
            .env("GIT_COMMITTER_NAME", "tripwire")
            .env("GIT_COMMITTER_EMAIL", "tripwire@example.invalid")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?} failed");
    };
    git(&["init", "--initial-branch=main"]);
    fs::write(dir.join("README.md"), "outcome fixture\n").expect("seed a file");
    git(&["add", "README.md"]);
    git(&["-c", "commit.gpgsign=false", "commit", "-m", "seed"]);
}

fn seed_isolated_home(home: &Path) {
    let base = home.join(".agents-in-a-box");
    let cfg = base.join("config");
    fs::create_dir_all(&cfg).expect("create isolated config dir");
    fs::write(
        cfg.join("onboarding.toml"),
        format!(
            "completed = true\n\
             completed_at = \"2026-09-04T00:00:00+00:00\"\n\
             version = \"{ver}\"\n\
             skipped_dependencies = []\n\
             git_directories = []\n",
            ver = env!("CARGO_PKG_VERSION"),
        ),
    )
    .expect("seed onboarding.toml");
    fs::write(
        base.join("install.json"),
        r#"{"agents":[],"hook_script":"","claude_plugin_dir":null,"codex_hooks_json":null,"plugin_version":null,"prompt_dismissed":true}"#,
    )
    .expect("seed install.json");
}

/// Three registry rows: TWO on the shared worktree, which is what makes an
/// answer there ambiguous, and one on its own.
fn seed_sessions(home: &Path, rows: &[(&str, &Path, &str, &str)]) {
    let mut sessions = serde_json::Map::new();
    for (tmux, worktree, workspace, uuid) in rows {
        sessions.insert(
            (*tmux).to_string(),
            json!({
                "session_id": uuid,
                "tmux_session_name": tmux,
                "worktree_path": worktree,
                "workspace_name": workspace,
                "created_at": "2026-09-04T00:00:00Z",
                "agent_type": "Claude",
                "skip_permissions": true,
            }),
        );
    }
    let registry = json!({ "sessions": sessions });
    fs::write(
        home.join(".agents-in-a-box").join("sessions.json"),
        serde_json::to_vec_pretty(&registry).expect("encode sessions.json"),
    )
    .expect("seed sessions.json");
}

fn seed_waiting_hook(home: &Path, cwd: &Path, project: &str, session_id: &str, question: &str) {
    let paths = Paths::under(home.join(".agents-in-a-box"));
    fs::create_dir_all(&paths.base).expect("create notifyd base");
    let store = Store::open(&paths.db).expect("open notifications.db");
    store
        .insert_and_prune(
            &Envelope {
                protocol_version: 1,
                agent: "claude".into(),
                raw_event: "Notification:idle_prompt".into(),
                session_id: session_id.into(),
                cwd: cwd.to_string_lossy().into_owned(),
                project: project.into(),
                ts: chrono::Utc::now().timestamp_millis() - 30_000,
                payload: json!({ "message": question }),
            },
            &RetentionPolicy {
                retention_days: 0,
                max_rows: 0,
            },
        )
        .expect("seed the waiting hook row");
}

const AMBIGUOUS_QUESTION: &str = "Which sqlite path?";
const SOLO_QUESTION: &str = "Rebase or merge here?";

#[test]
fn a_failed_answer_stays_on_its_own_question_across_a_navigation() {
    if !tmux_available() {
        eprintln!("SKIP: tmux not available");
        return;
    }

    let home_tmp = tempfile::Builder::new()
        .prefix("ainb-outcome-")
        .tempdir_in("/tmp")
        .expect("home tempdir");
    let home = home_tmp.path();
    seed_isolated_home(home);

    let pid = std::process::id();
    // The SHARED worktree carries two live sessions, which is what makes an
    // answer to its question ambiguous and so makes the send fail for real.
    let shared_tree = home.join("shared-worktree");
    let solo_tree = home.join("solo-worktree");
    for tree in [&shared_tree, &solo_tree] {
        fs::create_dir_all(tree).expect("create worktree dir");
        init_git_repo(tree);
    }
    let shared_a = format!("tmux_outcome_shareda_{pid}");
    let shared_b = format!("tmux_outcome_sharedb_{pid}");
    let solo = format!("tmux_outcome_solo_{pid}");
    seed_sessions(
        home,
        &[
            (
                &shared_a,
                &shared_tree,
                "shared-one",
                "6f1f5f7e-0000-4000-8000-0000000000d1",
            ),
            (
                &shared_b,
                &shared_tree,
                "shared-two",
                "6f1f5f7e-0000-4000-8000-0000000000d2",
            ),
            (
                &solo,
                &solo_tree,
                "solo-target",
                "6f1f5f7e-0000-4000-8000-0000000000d3",
            ),
        ],
    );
    seed_waiting_hook(
        home,
        &shared_tree,
        "shared-one",
        "outcome-shared",
        AMBIGUOUS_QUESTION,
    );
    seed_waiting_hook(
        home,
        &solo_tree,
        "solo-target",
        "outcome-solo",
        SOLO_QUESTION,
    );

    // Every one of them must be live: discovery filters on `is_running`, and a
    // session it drops is a session the operator cannot answer.
    let _panes: Vec<ExactTmuxSession> = [&shared_a, &shared_b, &solo]
        .into_iter()
        .map(|name| ExactTmuxSession::create(name.clone(), &["sh", "-c", "cat"]))
        .collect();

    let tui_tmux = format!("tripwire-outcome-{pid}");
    let tui = ExactTmuxSession::create(tui_tmux.clone(), &[]);
    let launch = format!(
        "HOME={home} AINB_DISABLE_PLUGINS=1 exec {bin} tui",
        home = home.display(),
        bin = ainb_bin().display()
    );
    assert!(
        Command::new("tmux")
            .args(["send-keys", "-t", &tui_tmux, &launch, "Enter"])
            .status()
            .expect("launch ainb tui")
            .success(),
        "tmux refused the launch command"
    );

    assert!(
        poll(&tui_tmux, Instant::now() + Duration::from_secs(90), |c| {
            c.contains("Sessions") && c.contains("[s]")
        })
        .is_some(),
        "HomeScreen never rendered:\n{}",
        capture_pane(&tui_tmux)
    );

    assert!(
        poll_until(
            &tui_tmux,
            "s",
            Instant::now() + Duration::from_secs(90),
            |c| c.contains("Workspaces ("),
            // The header pluralises: one waiting session says "needs you",
            // several say "N need you". Matching only the singular walked the
            // whole 90s deadline past a screen that was already correct.
            |c| c.contains("ASK") && (c.contains("need you") || c.contains("needs you")),
        )
        .is_some(),
        "no ASK chip on the sessions screen:\n{}",
        capture_pane(&tui_tmux)
    );

    // Walk to the ambiguous question rather than assuming which row the list
    // opens on. Its ask pane is where the failing answer is typed.
    let ask = focus_question(&tui_tmux, AMBIGUOUS_QUESTION, "Down", 5);
    assert!(
        ask.contains(AMBIGUOUS_QUESTION),
        "wrong question on screen:\n{ask}"
    );

    for key in ["d", "a", "t", "a", "-", "d", "i", "r"] {
        send_key(&tui_tmux, key);
        thread::sleep(Duration::from_millis(60));
    }
    send_key(&tui_tmux, "Enter");

    // The send is refused, and the pane says so against the question it failed
    // on. Two live sessions share this cwd, so typing the answer into either
    // pane would be a guess.
    let failed = poll(&tui_tmux, Instant::now() + Duration::from_secs(30), |c| {
        c.contains("not answered")
    });
    assert!(
        failed.is_some(),
        "an ambiguous target must report a refusal, not a spinner:\n{}",
        capture_pane(&tui_tmux)
    );

    // On to the other waiting question.
    let others_ask = focus_question(&tui_tmux, SOLO_QUESTION, "Down", 5);

    // THE assertion. The second question was never answered, so its pane must
    // report nothing — a failure carried over from the first would be this
    // surface telling the operator that a question they never sent an answer
    // to had failed.
    assert!(
        !others_ask.contains("not answered"),
        "a question nobody answered must not be shown as failed — this is the \
         first question's outcome painted under the second:\n{others_ask}"
    );

    // And back. The failure belongs to the first question and is still there,
    // which is what makes the retry the pane offers mean anything.
    let returned = focus_question(&tui_tmux, AMBIGUOUS_QUESTION, "Up", 5);
    assert!(
        returned.contains("not answered"),
        "the failure must survive the round trip: an outcome cleared by walking \
         away is an answer that silently evaporated:\n{returned}"
    );

    drop(tui);
}
