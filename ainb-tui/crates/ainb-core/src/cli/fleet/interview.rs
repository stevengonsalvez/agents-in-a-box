// ABOUTME: `ainb fleet interview list|release|surface` — the non-GUI lever for
// Claude's AskUserQuestion surface.
//
// Releasing a held interview previously required a key in the Hangar Fleet
// screen, which is useless in the one case that matters: the pane you need to
// unblock IS your terminal, and while the hook holds the tool call Claude owns
// its stdin, so nothing typed inside the session can free it. The release has
// to arrive from outside the pane — this verb, or a tmux binding that calls it.

use std::path::Path;

use anyhow::{Context, Result};

use crate::cli::OutputFormat;
use crate::cli::fleet::atc::{InterviewSurface, interview_surface, set_interview_surface};
use ainb_plugin_notifyd::broker::{client_list, client_release_structured};

/// One interview currently held open by a blocked hook.
#[derive(serde::Serialize)]
struct HeldRow {
    session_id: String,
    request_fingerprint: String,
    waiting_secs: u64,
    surface: String,
}

pub async fn execute(matches: &clap::ArgMatches, format: OutputFormat) -> Result<()> {
    let home = crate::fleet::plumbing::paths::ainb_home().context("resolving ainb home")?;
    match matches.subcommand() {
        Some(("list", _)) => list(&home, format),
        Some(("release", sub)) => {
            release(&home, sub.get_one::<String>("session").map(String::as_str))
        }
        Some(("surface", sub)) => surface(
            &home,
            sub.get_one::<String>("mode").map(String::as_str),
            sub.get_one::<String>("session").map(String::as_str),
        ),
        _ => Ok(()),
    }
}

fn socket(home: &Path) -> std::path::PathBuf {
    ainb_plugin_notifyd::paths::Paths::under(home).approve_socket
}

fn held(home: &Path) -> Result<Vec<HeldRow>> {
    let sock = socket(home);
    // A broker we cannot reach is NOT "nothing is held" — a hook may be parked
    // right now. Saying "no interviews are being held open" to someone whose
    // session is blocked is the same lie as reporting a failed delivery as
    // delivered, so this fails loudly like its sibling `approve` verb does.
    Ok(client_list(&sock)
        .with_context(|| {
            format!(
                "approve broker unreachable at {} (repair with `ainb notifyd restart`)",
                sock.display()
            )
        })?
        .into_iter()
        .filter(|p| p.tool == "AskUserQuestion")
        .map(|p| HeldRow {
            surface: interview_surface(home, &p.session_id).token().to_string(),
            session_id: p.session_id,
            request_fingerprint: p.request_fingerprint.unwrap_or_default(),
            waiting_secs: p.waiting_ms / 1000,
        })
        .collect())
}

/// Strip control characters. These strings arrive off the socket from whoever
/// registered the request, and are printed straight to a terminal — the sibling
/// `approve` verb sanitises the same class of value for the same reason.
fn sanitize(value: &str) -> String {
    value.chars().map(|c| if c.is_control() { ' ' } else { c }).collect()
}

fn list(home: &Path, format: OutputFormat) -> Result<()> {
    let rows = held(home)?;
    if matches!(format, OutputFormat::Json) {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if rows.is_empty() {
        println!("no interviews are being held open");
        println!(
            "(default surface: {} — set with `ainb fleet interview surface native|fleet`)",
            interview_surface(home, "").token()
        );
        return Ok(());
    }
    println!("{:<38} {:>8}  {}", "SESSION", "WAITING", "FINGERPRINT");
    for r in &rows {
        println!(
            "{:<38} {:>7}s  {}",
            sanitize(&r.session_id),
            r.waiting_secs,
            sanitize(&r.request_fingerprint)
        );
    }
    println!("\nrelease to the terminal: ainb fleet interview release <session>");
    Ok(())
}

fn release(home: &Path, session: Option<&str>) -> Result<()> {
    let rows = held(home)?;
    let pane_session = crate::cli::fleet::atc::pane_held_session(home);
    let row = match session {
        Some(id) => rows.iter().find(|r| r.session_id == id).with_context(|| {
            format!("session {id} is not holding an interview (see `ainb fleet interview list`)")
        })?,
        // Bound to `prefix + A`, this runs INSIDE the held pane, so the pane's
        // own session is the only sane target: releasing "the only held one"
        // would hand somebody else's question back to a terminal nobody is
        // watching the moment two are held at once.
        None => match pane_session {
            // Inside a tmux pane the pane's OWN interview is the only safe
            // target. Falling back to "the only held one" here would release
            // somebody else's question into a terminal nobody is watching,
            // which is exactly what the pane-aware lookup exists to prevent.
            Some(id) => rows
                .iter()
                .find(|r| r.session_id == id)
                .with_context(|| "this pane is not holding an interview".to_string())?,
            None if rows.is_empty() => anyhow::bail!("no interviews are being held open"),
            None if rows.len() == 1 => &rows[0],
            None => anyhow::bail!(
                "{} interviews are held and this is not a tmux pane; name one \
                 (see `ainb fleet interview list`)",
                rows.len()
            ),
        },
    };
    let ack = client_release_structured(&socket(home), &row.session_id, &row.request_fingerprint)
        .context("releasing the held interview")?;
    if ack.matched {
        println!(
            "released {} — Claude's picker is now in its terminal",
            row.session_id
        );
    } else {
        println!(
            "{} was not holding that request any more (already answered or released)",
            sanitize(&row.session_id)
        );
    }
    Ok(())
}

fn surface(home: &Path, mode: Option<&str>, session: Option<&str>) -> Result<()> {
    let Some(mode) = mode else {
        match session {
            Some(id) => println!("{id}: {}", interview_surface(home, id).token()),
            None => println!("default: {}", interview_surface(home, "").token()),
        }
        return Ok(());
    };
    let target = if mode == "fleet" {
        InterviewSurface::Fleet
    } else {
        InterviewSurface::Native
    };
    set_interview_surface(home, session, target)?;
    match session {
        Some(id) => println!("{id}: {}", target.token()),
        None => println!(
            "default: {} (persisted to config.toml under [fleet.interview])",
            target.token()
        ),
    }
    Ok(())
}
