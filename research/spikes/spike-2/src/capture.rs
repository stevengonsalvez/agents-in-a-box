//! The two feeds under test: tmux control mode, and a direct pty.

use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

/// Run `sh <script>` on a real pty of the given size and return every byte the
/// program wrote, up to EOF.
pub fn capture_pty(
    script: &Path,
    cols: u16,
    rows: u16,
    timeout: Duration,
) -> Result<(Vec<u8>, Vec<usize>)> {
    use portable_pty::{native_pty_system, CommandBuilder, PtySize};

    let pty = native_pty_system()
        .openpty(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 })
        .context("openpty")?;
    let mut cmd = CommandBuilder::new("sh");
    cmd.arg(script.to_str().unwrap());
    cmd.env("TERM", "xterm-256color");
    cmd.env("LANG", "C.UTF-8");
    cmd.env("LC_ALL", "C.UTF-8");
    let mut child = pty.slave.spawn_command(cmd).context("spawn on pty")?;
    drop(pty.slave);
    let mut reader = pty.master.try_clone_reader().context("clone reader")?;

    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let mut sizes = Vec::new();
        let mut chunk = [0u8; 65536];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    sizes.push(n);
                }
            }
        }
        let _ = tx.send((buf, sizes));
    });

    let out = rx.recv_timeout(timeout).unwrap_or_default();
    let _ = child.kill();
    let _ = child.wait();
    Ok(out)
}

pub struct TmuxCapture {
    /// Pane bytes, octal-decoded and concatenated in arrival order.
    pub bytes: Vec<u8>,
    /// The control stream exactly as tmux wrote it.
    pub raw: Vec<u8>,
    pub pauses: usize,
    pub continues: usize,
    pub extended: usize,
    pub plain: usize,
    pub max_age_ms: u64,
    /// Payload length of each  /  notification, in
    /// arrival order: the real chunk boundaries a daemon parser would see.
    pub chunks: Vec<usize>,
}

/// Drive `sh <script>` inside a private tmux server and read it back through a
/// control-mode client, exactly as the R2 daemon would.
pub fn capture_tmux(
    socket: &str,
    session: &str,
    script: &Path,
    cols: u16,
    rows: u16,
    timeout: Duration,
) -> Result<TmuxCapture> {
    let go = script.with_extension("go");
    let _ = std::fs::remove_file(&go);

    let t = |args: &[&str]| -> Result<String> {
        let out = Command::new("tmux")
            .args(["-L", socket, "-f", "/dev/null"])
            .args(args)
            .output()
            .context("tmux")?;
        if !out.status.success() {
            bail!("tmux {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
        }
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    };

    let _ = t(&["kill-session", "-t", session]);
    let boot = format!(
        "sh -c 'while [ ! -f {} ]; do sleep 0.02; done; exec sh {}'",
        go.display(),
        script.display()
    );
    t(&[
        "new-session", "-d", "-s", session,
        "-x", &cols.to_string(), "-y", &rows.to_string(),
        "-n", "w0",
        "-e", "TERM=xterm-256color", "-e", "LANG=C.UTF-8", "-e", "LC_ALL=C.UTF-8",
        &boot,
    ])?;
    // Spike 1: only `window-size manual` pins the geometry on tmux 3.4.
    t(&["set-option", "-g", "window-size", "manual"])?;
    t(&["set-option", "-g", "default-terminal", "xterm-256color"])?;
    t(&["resize-window", "-t", session, "-x", &cols.to_string(), "-y", &rows.to_string()])?;
    let pane = t(&["display", "-p", "-t", session, "#{pane_id}"])?;

    let mut child: Child = Command::new("tmux")
        .args(["-L", socket, "-f", "/dev/null", "-C", "attach-session", "-t", session])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("spawn control client")?;

    let mut stdin = child.stdin.take().unwrap();
    // Spike 7: the pane id must be quoted, a bare `%0` is a tmux parse error.
    write!(
        stdin,
        "refresh-client -C {cols}x{rows}\nrefresh-client -f pause-after=2\nrefresh-client -A \"{pane}:on\"\n"
    )?;
    stdin.flush()?;

    let go2 = go.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400));
        let _ = std::fs::write(&go2, b"go");
    });

    let mut stdout = child.stdout.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut v = Vec::new();
        let _ = stdout.read_to_end(&mut v);
        let _ = tx.send(v);
    });

    let raw: Vec<u8> = match rx.recv_timeout(timeout) {
        Ok(s) => s,
        Err(_) => {
            let _ = child.kill();
            bail!("control client did not finish within {timeout:?}");
        }
    };
    drop(stdin);
    let _ = child.wait();
    let _ = t(&["kill-session", "-t", session]);
    let _ = std::fs::remove_file(&go);

    let mut cap = TmuxCapture {
        bytes: Vec::new(),
        raw: raw.clone(),
        pauses: 0,
        continues: 0,
        extended: 0,
        plain: 0,
        max_age_ms: 0,
        chunks: Vec::new(),
    };
    for line in raw.split(|b| *b == b'\n') {
        if let Some((kind, age, data)) = split_notification(line) {
            match kind {
                Notification::Extended => {
                    cap.extended += 1;
                    cap.max_age_ms = cap.max_age_ms.max(age);
                }
                Notification::Plain => cap.plain += 1,
            }
            let d = unvis(data);
            cap.chunks.push(d.len());
            cap.bytes.extend_from_slice(&d);
        } else if line.starts_with(b"%pause ") {
            cap.pauses += 1;
        } else if line.starts_with(b"%continue ") {
            cap.continues += 1;
        }
    }
    Ok(cap)
}

/// Reverse tmux's `vis(3)`-style escaping: any byte tmux considers unprintable
/// arrives as a backslash plus exactly three octal digits. UTF-8 passes through
/// verbatim, and tmux will happily end a notification in the middle of a
/// multi-byte grapheme, so this works on bytes and never on `str`.
pub fn unvis(b: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(|c| (b'0'..=b'7').contains(c)) {
            let v = (b[i + 1] - b'0') as u32 * 64 + (b[i + 2] - b'0') as u32 * 8 + (b[i + 3] - b'0') as u32;
            out.push(v as u8);
            i += 4;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

pub enum Notification {
    Plain,
    Extended,
}

/// Split one control-mode line into its notification kind, buffering age and
/// raw (still escaped) payload. Byte-oriented: tmux ends notifications mid
/// grapheme, so a `str` parse would reject valid frames.
pub fn split_notification(line: &[u8]) -> Option<(Notification, u64, &[u8])> {
    fn field(b: &[u8]) -> (&[u8], &[u8]) {
        match b.iter().position(|c| *c == b' ') {
            Some(i) => (&b[..i], &b[i + 1..]),
            None => (b, &b[..0]),
        }
    }
    if let Some(rest) = line.strip_prefix(b"%extended-output ") {
        // %extended-output %<pane> <age> : <data>
        let (_pane, rest) = field(rest);
        let (age, rest) = field(rest);
        let age: u64 = std::str::from_utf8(age).ok()?.parse().ok()?;
        let data = rest.strip_prefix(b": ").or_else(|| rest.strip_prefix(b":")).unwrap_or(rest);
        return Some((Notification::Extended, age, data));
    }
    if let Some(rest) = line.strip_prefix(b"%output ") {
        let (_pane, data) = field(rest);
        return Some((Notification::Plain, 0, data));
    }
    None
}
