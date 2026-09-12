
//! Live control-mode session: attach to an already-running tmux session, feed a
//! VT emulator from `%extended-output`, and settle the comparison inside the
//! same ordered stream by asking tmux for `capture-pane` and diffing its answer
//! against the emulator's grid at the point the reply arrives.

use crate::backends;
use anyhow::{Context, Result};
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct LiveReport {
    pub captures: Vec<CaptureCmp>,
    pub bytes: usize,
    pub notifications: usize,
    pub pauses: usize,
    pub continues: usize,
    pub max_age_ms: u64,
    pub feed_cpu_s: f64,
    pub wall_s: f64,
    pub dumps: Vec<(String, String)>,
}

pub struct CaptureCmp {
    pub label: String,
    pub rows: usize,
    pub mismatched: Vec<(usize, String, String)>,
    pub emu_text: Vec<String>,
    pub tmux_text: Vec<String>,
}

/// The terminal state tmux keeps outside the character grid, which
/// `capture-pane -e` does not express and a re-snapshot has to restore.
#[derive(Default)]
struct SeedState {
    alt: bool,
    cursor_visible: bool,
    cursor_y: usize,
    cursor_x: usize,
    region: (usize, usize),
    app_cursor: bool,
    app_keypad: bool,
    wrap: bool,
    origin: bool,
    insert: bool,
    /// standard, button, any, sgr, utf8
    mouse: [bool; 5],
}

/// One step of the driving script.
enum Step {
    Sleep(u64),
    Cmd(String),
    Capture(String),
    /// R2's snapshot rule: rebuild the emulator from `capture-pane -e` and only
    /// then resume applying deltas.
    Seed,
    /// Write the whole canonical snapshot, not just its text rows, so state the
    /// text comparison cannot see (alt screen, OSC 8, title, mouse mode) is
    /// visible too.
    Dump(String),
}

fn parse_steps(src: &str) -> Vec<Step> {
    src.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| match l.split_once(' ') {
            Some(("sleep", v)) => Step::Sleep(v.trim().parse().unwrap_or(0)),
            Some(("cmd", v)) => Step::Cmd(v.to_string()),
            Some(("capture", v)) => Step::Capture(v.to_string()),
            None if l == "seed" => Step::Seed,
            Some(("dump", v)) => Step::Dump(v.to_string()),
            _ => Step::Cmd(l.to_string()),
        })
        .collect()
}

fn cpu_secs() -> f64 {
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let close = s.rfind(')').unwrap_or(0);
    let f: Vec<&str> = s[close + 2..].split_whitespace().collect();
    let ut: f64 = f.get(11).and_then(|x| x.parse().ok()).unwrap_or(0.0);
    let st: f64 = f.get(12).and_then(|x| x.parse().ok()).unwrap_or(0.0);
    (ut + st) / 100.0
}

pub fn run(
    socket: &str,
    session: &str,
    pane: &str,
    cols: usize,
    rows: usize,
    backend: &str,
    steps_src: &str,
    raw_out: Option<&std::path::Path>,
) -> Result<LiveReport> {
    let steps = parse_steps(steps_src);
    let mut child = Command::new("tmux")
        .args(["-L", socket, "-f", "/dev/null", "-C", "attach-session", "-t", session])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn control client")?;
    let mut stdin = child.stdin.take().unwrap();
    write!(
        stdin,
        "refresh-client -C {cols}x{rows}\nrefresh-client -f pause-after=2\nrefresh-client -A \"{pane}:on\"\n"
    )?;
    stdin.flush()?;

    // The writer thread runs the script; the reader below stays in lockstep with
    // tmux's own ordering of %output and command replies.
    let pane_owned = pane.to_string();
    let trace = std::env::var("SPIKE2_TRACE").is_ok();
    let steps_thread = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(200));
        for s in steps {
            match s {
                Step::Sleep(ms) => {
                    if trace { eprintln!("[w] sleep {ms}"); }
                    std::thread::sleep(Duration::from_millis(ms));
                }
                Step::Cmd(c) => {
                    if trace { eprintln!("[w] cmd {c}"); }
                    let _ = writeln!(stdin, "{c}");
                    let _ = stdin.flush();
                }
                Step::Capture(label) => {
                    if trace { eprintln!("[w] capture {label}"); }
                    // Marker first, then the capture. Both replies arrive in the
                    // same ordered stream as %output, so the reader knows the
                    // emulator has consumed exactly the bytes tmux had already
                    // sent when it rendered the grid.
                    let _ = writeln!(stdin, "display-message -p \"SPIKE2CAP {label}\"");
                    let _ = writeln!(stdin, "capture-pane -p -N -t {pane_owned}");
                    let _ = stdin.flush();
                    std::thread::sleep(Duration::from_millis(100));
                }
                Step::Dump(label) => {
                    if trace { eprintln!("[w] dump {label}"); }
                    let _ = writeln!(stdin, "display-message -p \"SPIKE2DUMP {label}\"");
                    let _ = writeln!(
                        stdin,
                        "display-message -p -t {pane_owned} \"alt=#{{alternate_on}} title=#{{pane_title}}\""
                    );
                    let _ = stdin.flush();
                    std::thread::sleep(Duration::from_millis(100));
                }
                Step::Seed => {
                    if trace { eprintln!("[w] seed"); }
                    // A complete re-snapshot needs three replies, in this order:
                    // the terminal state tmux keeps outside the grid, the pane
                    // title, and the grid itself. capture-pane -e carries SGR
                    // and OSC 8 but nothing else, so the rest comes from formats.
                    let _ = writeln!(stdin, "display-message -p \"SPIKE2SEED\"");
                    let _ = writeln!(
                        stdin,
                        "display-message -p -t {pane_owned} \"#{{alternate_on}} #{{cursor_flag}} #{{cursor_y}} #{{cursor_x}} #{{scroll_region_upper}} #{{scroll_region_lower}} #{{keypad_cursor_flag}} #{{keypad_flag}} #{{wrap_flag}} #{{origin_flag}} #{{insert_flag}} #{{mouse_standard_flag}} #{{mouse_button_flag}} #{{mouse_any_flag}} #{{mouse_sgr_flag}} #{{mouse_utf8_flag}}\""
                    );
                    let _ = writeln!(stdin, "display-message -p -t {pane_owned} \"#{{pane_title}}\"");
                    let _ = writeln!(stdin, "capture-pane -p -e -t {pane_owned}");
                    let _ = stdin.flush();
                    std::thread::sleep(Duration::from_millis(150));
                }
            }
        }
        if trace { eprintln!("[w] steps done, killing session"); }
        std::thread::sleep(Duration::from_millis(300));
        let _ = writeln!(stdin, "kill-session");
        let _ = stdin.flush();
        std::thread::sleep(Duration::from_millis(500));
        drop(stdin);
    });

    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);
    let mut emu = backends::make(backend, cols, rows, 1000);
    let mut rep = LiveReport {
        captures: Vec::new(),
        bytes: 0,
        notifications: 0,
        pauses: 0,
        continues: 0,
        max_age_ms: 0,
        feed_cpu_s: 0.0,
        wall_s: 0.0,
        dumps: Vec::new(),
    };
    let mut raw: Vec<u8> = Vec::new();
    let mut in_block = false;
    let mut block: Vec<Vec<u8>> = Vec::new();
    let mut pending_label: Option<String> = None;
    let mut pending_seed: u8 = 0;
    let mut pending_dump: Option<String> = None;
    let mut seed_state = SeedState::default();
    let t0 = Instant::now();
    let mut feed_cpu = 0.0f64;
    // A deliberately slow reader, used to make tmux trip its own pause-after
    // threshold the way a backed-up viewer would.
    let read_delay_ms: u64 = std::env::var("SPIKE2_READ_DELAY_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);

    let trace_r = std::env::var("SPIKE2_TRACE").is_ok();
    let mut line: Vec<u8> = Vec::new();
    loop {
        line.clear();
        // Byte-oriented: tmux ends a notification in the middle of a multi-byte
        // grapheme under load, so a UTF-8 line reader would error out here.
        match reader.read_until(b'\n', &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        while line.last() == Some(&b'\n') || line.last() == Some(&b'\r') {
            line.pop();
        }
        let l: &[u8] = &line;
        if in_block {
            if l.starts_with(b"%end ") || l.starts_with(b"%error ") {
                in_block = false;
                let body = std::mem::take(&mut block);
                if let Some(first) = body.first() {
                    if let Some(lab) = first.strip_prefix(b"SPIKE2CAP ") {
                        pending_label = Some(String::from_utf8_lossy(lab).trim().to_string());
                        continue;
                    }
                    if first.as_slice() == b"SPIKE2SEED" {
                        pending_seed = 1;
                        continue;
                    }
                    if let Some(lab) = first.strip_prefix(b"SPIKE2DUMP ") {
                        pending_dump = Some(String::from_utf8_lossy(lab).trim().to_string());
                        continue;
                    }
                }
                if pending_seed == 1 {
                    let t = body.first().map(|b| String::from_utf8_lossy(b).to_string()).unwrap_or_default();
                    let f: Vec<usize> =
                        t.split_whitespace().map(|v| v.parse().unwrap_or(0)).collect();
                    let g = |i: usize| f.get(i).copied().unwrap_or(0);
                    seed_state = SeedState {
                        alt: g(0) == 1,
                        cursor_visible: g(1) == 1,
                        cursor_y: g(2),
                        cursor_x: g(3),
                        region: (g(4), g(5)),
                        app_cursor: g(6) == 1,
                        app_keypad: g(7) == 1,
                        wrap: g(8) == 1,
                        origin: g(9) == 1,
                        insert: g(10) == 1,
                        mouse: [g(11) == 1, g(12) == 1, g(13) == 1, g(14) == 1, g(15) == 1],
                    };
                    // Enter the right buffer before anything is painted into it.
                    emu.feed(if seed_state.alt { b"\x1b[?1049h" } else { b"\x1b[?1049l" });
                    pending_seed = 2;
                    continue;
                }
                if pending_seed == 2 {
                    let title = body.first().map(|b| String::from_utf8_lossy(b).to_string()).unwrap_or_default();
                    emu.feed(format!("\x1b]0;{}\x07", title.trim_end()).as_bytes());
                    pending_seed = 3;
                    continue;
                }
                if pending_seed == 3 {
                    // Grid: full-screen repaint with the scroll region and origin
                    // mode out of the way, one absolute-positioned row per line.
                    // capture-pane -e carries SGR and OSC 8, so both survive.
                    let mut repaint: Vec<u8> = b"\x1b[r\x1b[?6l\x1b[H\x1b[2J\x1b[0m".to_vec();
                    for (i, row) in body.iter().enumerate().take(rows) {
                        repaint.extend_from_slice(format!("\x1b[{};1H", i + 1).as_bytes());
                        repaint.extend_from_slice(row);
                        repaint.extend_from_slice(b"\x1b[0m");
                    }
                    let st = &seed_state;
                    // Scroll region, then modes, then the cursor last: DECSTBM
                    // homes the cursor and DECOM changes what CUP means.
                    repaint.extend_from_slice(
                        format!("\x1b[{};{}r", st.region.0 + 1, st.region.1 + 1).as_bytes(),
                    );
                    for (on, seq) in [
                        (st.mouse[0], "1000"),
                        (st.mouse[1], "1002"),
                        (st.mouse[2], "1003"),
                        (st.mouse[4], "1005"),
                        (st.mouse[3], "1006"),
                        (st.app_cursor, "1"),
                        (st.wrap, "7"),
                    ] {
                        repaint.extend_from_slice(
                            format!("\x1b[?{}{}", seq, if on { "h" } else { "l" }).as_bytes(),
                        );
                    }
                    repaint.extend_from_slice(if st.app_keypad { b"\x1b=" } else { b"\x1b>" });
                    repaint.extend_from_slice(if st.insert { b"\x1b[4h" } else { b"\x1b[4l" });
                    repaint.extend_from_slice(
                        format!("\x1b[{};{}H", st.cursor_y + 1, st.cursor_x + 1).as_bytes(),
                    );
                    if st.origin {
                        repaint.extend_from_slice(b"\x1b[?6h");
                    }
                    repaint.extend_from_slice(if st.cursor_visible { b"\x1b[?25h" } else { b"\x1b[?25l" });
                    emu.feed(&repaint);
                    pending_seed = 0;
                    continue;
                }
                if let Some(label) = pending_dump.take() {
                    let canon = emu.canon();
                    let tmux_state = body
                        .first()
                        .map(|b| String::from_utf8_lossy(b).trim().to_string())
                        .unwrap_or_default();
                    rep.dumps.push((label, format!("tmux {tmux_state}\n{}", canon.render())));
                    continue;
                }
                if let Some(label) = pending_label.take() {
                    let canon = emu.canon();
                    let emu_text: Vec<String> = canon
                        .render()
                        .lines()
                        .filter_map(|s| {
                            s.split_once(" t |")
                                .map(|(_, t)| t.trim_end_matches('|').trim_end().to_string())
                        })
                        .collect();
                    let mut tmux_text: Vec<String> = body
                        .iter()
                        .map(|s| String::from_utf8_lossy(s).trim_end().to_string())
                        .collect();
                    tmux_text.resize(rows, String::new());
                    let mut mismatched = Vec::new();
                    for i in 0..rows {
                        let a = emu_text.get(i).cloned().unwrap_or_default();
                        let b = tmux_text[i].clone();
                        if a != b {
                            mismatched.push((i, a, b));
                        }
                    }
                    rep.captures.push(CaptureCmp { label, rows, mismatched, emu_text, tmux_text });
                }
                continue;
            }
            // tmux delivers %pause and %continue INSIDE the reply block of the
            // command that caused them, so a parser that only looks at
            // top-level notifications never sees the resume.
            if l.starts_with(b"%pause ") {
                rep.pauses += 1;
                continue;
            }
            if l.starts_with(b"%continue ") {
                rep.continues += 1;
                continue;
            }
            block.push(l.to_vec());
            continue;
        }
        if trace_r && !l.starts_with(b"%output") && !l.starts_with(b"%extended-output") {
            eprintln!("[r] {}", String::from_utf8_lossy(&l[..l.len().min(60)]));
        }
        if l.starts_with(b"%begin ") {
            in_block = true;
            block.clear();
            continue;
        }
        if let Some((kind, age, data)) = crate::capture::split_notification(l) {
            if matches!(kind, crate::capture::Notification::Extended) {
                rep.max_age_ms = rep.max_age_ms.max(age);
            }
            let d = crate::capture::unvis(data);
            rep.bytes += d.len();
            rep.notifications += 1;
            if raw_out.is_some() {
                raw.extend_from_slice(&d);
            }
            let c0 = cpu_secs();
            emu.feed(&d);
            feed_cpu += cpu_secs() - c0;
            if read_delay_ms > 0 {
                std::thread::sleep(Duration::from_millis(read_delay_ms));
            }
        } else if l.starts_with(b"%pause ") {
            rep.pauses += 1;
        } else if l.starts_with(b"%continue ") {
            rep.continues += 1;
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    let _ = steps_thread.join();
    if let Some(p) = raw_out {
        std::fs::write(p, &raw)?;
    }
    rep.feed_cpu_s = feed_cpu;
    rep.wall_s = t0.elapsed().as_secs_f64();
    Ok(rep)
}
