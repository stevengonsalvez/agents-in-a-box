//! Spike 2 harness: does a Rust VT emulator fed by tmux control mode reproduce
//! a live screen byte-equal to the same program driven through a direct pty?

mod backends;
mod canon;
mod capture;
mod live;

use anyhow::{bail, Result};
use std::io::Read;
use std::path::PathBuf;
use std::time::{Duration, Instant};

fn rss_kb() -> u64 {
    let s = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    for line in s.lines() {
        if let Some(v) = line.strip_prefix("VmRSS:") {
            return v.trim().trim_end_matches(" kB").trim().parse().unwrap_or(0);
        }
    }
    0
}

fn cpu_secs() -> f64 {
    // utime + stime of this process, in seconds.
    let s = std::fs::read_to_string("/proc/self/stat").unwrap_or_default();
    let close = s.rfind(')').unwrap_or(0);
    let f: Vec<&str> = s[close + 2..].split_whitespace().collect();
    let hz = 100.0;
    let ut: f64 = f.get(11).and_then(|x| x.parse().ok()).unwrap_or(0.0);
    let st: f64 = f.get(12).and_then(|x| x.parse().ok()).unwrap_or(0.0);
    (ut + st) / hz
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(String::as_str).unwrap_or("");
    match cmd {
        "capture-pty" => {
            // capture-pty <script> <cols> <rows> <out.bin>
            let script = PathBuf::from(&args[2]);
            let cols: u16 = args[3].parse()?;
            let rows: u16 = args[4].parse()?;
            let out = PathBuf::from(&args[5]);
            let (b, chunks) = capture::capture_pty(&script, cols, rows, Duration::from_secs(60))?;
            std::fs::write(&out, &b)?;
            std::fs::write(
                out.with_extension("chunks"),
                chunks.iter().map(|n| n.to_string()).collect::<Vec<_>>().join("
"),
            )?;
            println!("pty bytes={} reads={} -> {}", b.len(), chunks.len(), out.display());
        }
        "capture-tmux" => {
            // capture-tmux <socket> <session> <script> <cols> <rows> <out-prefix>
            let socket = &args[2];
            let session = &args[3];
            let script = PathBuf::from(&args[4]);
            let cols: u16 = args[5].parse()?;
            let rows: u16 = args[6].parse()?;
            let prefix = PathBuf::from(&args[7]);
            let c = capture::capture_tmux(socket, session, &script, cols, rows, Duration::from_secs(60))?;
            std::fs::write(prefix.with_extension("bin"), &c.bytes)?;
            std::fs::write(prefix.with_extension("ctl"), &c.raw)?;
            std::fs::write(
                prefix.with_extension("chunks"),
                c.chunks.iter().map(|n| n.to_string()).collect::<Vec<_>>().join("
"),
            )?;
            println!(
                "tmux bytes={} extended={} plain={} pauses={} continues={} max_age_ms={}",
                c.bytes.len(), c.extended, c.plain, c.pauses, c.continues, c.max_age_ms
            );
        }
        "snapshot" => {
            // snapshot <in.bin> <cols> <rows> <backend> [scrollback]
            let bin = std::fs::read(&args[2])?;
            let cols: usize = args[3].parse()?;
            let rows: usize = args[4].parse()?;
            let backend = &args[5];
            let sb: usize = args.get(6).map(|s| s.parse().unwrap()).unwrap_or(1000);
            // An optional chunk-size file replays the feed's real arrival
            // boundaries, so a parser that mishandles a split escape sequence
            // or a split UTF-8 grapheme shows up here.
            let chunks: Vec<usize> = match args.get(7) {
                Some(path) => std::fs::read_to_string(path)?
                    .lines()
                    .filter_map(|l| l.trim().parse().ok())
                    .collect(),
                None => Vec::new(),
            };
            let mut e = backends::make(backend, cols, rows, sb);
            if chunks.is_empty() {
                for c in bin.chunks(4096) {
                    e.feed(c);
                }
            } else {
                let mut off = 0usize;
                for n in chunks {
                    let end = (off + n).min(bin.len());
                    e.feed(&bin[off..end]);
                    off = end;
                }
                if off < bin.len() {
                    e.feed(&bin[off..]);
                }
            }
            print!("{}", e.canon().render());
        }
        "bench-rss" => {
            // bench-rss <backend> <n> <in.bin> <cols> <rows> <scrollback>
            let backend = &args[2];
            let n: usize = args[3].parse()?;
            let bin = std::fs::read(&args[4])?;
            let cols: usize = args[5].parse()?;
            let rows: usize = args[6].parse()?;
            let sb: usize = args[7].parse()?;
            let before = rss_kb();
            let mut all: Vec<Box<dyn backends::Emu>> = Vec::with_capacity(n);
            for _ in 0..n {
                let mut e = backends::make(backend, cols, rows, sb);
                for c in bin.chunks(4096) {
                    e.feed(c);
                }
                all.push(e);
            }
            let after = rss_kb();
            let checksum: usize = all.iter().map(|e| e.canon().render().len()).sum();
            let peak = rss_kb();
            println!(
                "backend={backend} n={n} workload_bytes={} rss_before_kb={before} rss_after_kb={after} rss_peak_kb={peak} delta_kb={} per_emu_kb={:.1} checksum={checksum}",
                bin.len(),
                after - before,
                (after - before) as f64 / n as f64
            );
            std::hint::black_box(&all);
        }
        "bench-cpu" => {
            // bench-cpu <backend> <in.bin> <cols> <rows> <scrollback>
            let backend = &args[2];
            let bin = std::fs::read(&args[3])?;
            let cols: usize = args[4].parse()?;
            let rows: usize = args[5].parse()?;
            let sb: usize = args[6].parse()?;
            let mut e = backends::make(backend, cols, rows, sb);
            let c0 = cpu_secs();
            let t0 = Instant::now();
            for c in bin.chunks(65536) {
                e.feed(c);
            }
            let dump = e.canon().render();
            let wall = t0.elapsed().as_secs_f64();
            let cpu = cpu_secs() - c0;
            println!(
                "backend={backend} bytes={} wall_s={:.3} cpu_s={:.3} MB_per_s={:.1} dump_len={}",
                bin.len(), wall, cpu,
                bin.len() as f64 / 1e6 / wall.max(1e-9),
                dump.len()
            );
        }
        "live" => {
            // live <socket> <session> <pane> <cols> <rows> <backend> <steps-file> <out-prefix>
            let socket = &args[2];
            let session = &args[3];
            let pane = &args[4];
            let cols: usize = args[5].parse()?;
            let rows: usize = args[6].parse()?;
            let backend = &args[7];
            let steps = std::fs::read_to_string(&args[8])?;
            let prefix = PathBuf::from(&args[9]);
            let raw = prefix.with_extension("raw");
            // The 50 MB flood does not need a 50 MB copy on disk.
            let keep_raw = std::env::var("SPIKE2_NO_RAW").is_err();
            let rep = live::run(
                socket, session, pane, cols, rows, backend, &steps,
                if keep_raw { Some(raw.as_path()) } else { None },
            )?;
            println!(
                "backend={backend} bytes={} notifications={} pauses={} continues={} max_age_ms={} feed_cpu_s={:.3} wall_s={:.3}",
                rep.bytes, rep.notifications, rep.pauses, rep.continues, rep.max_age_ms,
                rep.feed_cpu_s, rep.wall_s
            );
            for (label, body) in &rep.dumps {
                std::fs::write(prefix.with_extension(format!("{label}.dump")), body)?;
                println!("dump {label} -> {}", prefix.with_extension(format!("{label}.dump")).display());
            }
            for c in &rep.captures {
                println!("capture {} rows={} mismatched={}", c.label, c.rows, c.mismatched.len());
                let mut detail = String::new();
                for (i, a, b) in &c.mismatched {
                    detail.push_str(&format!("  r{i:03} emu |{a}|
  r{i:03} tmx |{b}|
"));
                }
                std::fs::write(prefix.with_extension(format!("{}.emu", c.label)), c.emu_text.join("
"))?;
                std::fs::write(prefix.with_extension(format!("{}.tmx", c.label)), c.tmux_text.join("
"))?;
                std::fs::write(prefix.with_extension(format!("{}.diff", c.label)), detail)?;
            }
        }
        "attach-client" => {
            // attach-client <socket> <session> <cols> <rows> <seconds>
            // A second, ordinary rendered client, used to prove what the
            // daemon's control client does and does not do to window geometry.
            use portable_pty::{native_pty_system, CommandBuilder, PtySize};
            let socket = args[2].clone();
            let session = args[3].clone();
            let cols: u16 = args[4].parse()?;
            let rows: u16 = args[5].parse()?;
            let secs: u64 = args[6].parse()?;
            let pty = native_pty_system().openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })?;
            let mut cmd = CommandBuilder::new("tmux");
            cmd.args(["-L", &socket, "-f", "/dev/null", "attach-session", "-t", &session]);
            cmd.env("TERM", "xterm-256color");
            let mut child = pty.slave.spawn_command(cmd)?;
            drop(pty.slave);
            let mut reader = pty.master.try_clone_reader()?;
            std::thread::spawn(move || {
                let mut buf = [0u8; 65536];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                }
            });
            println!("attached {cols}x{rows}");
            std::thread::sleep(Duration::from_secs(secs));
            let _ = child.kill();
            let _ = child.wait();
        }
        "cursor-col" => {
            // cursor-col <in.bin> <cols> <rows> <backend>
            // Reports the cursor column the emulator ends on, which is the
            // exact cell advance of whatever the fixture just printed.
            let bin = std::fs::read(&args[2])?;
            let cols: usize = args[3].parse()?;
            let rows: usize = args[4].parse()?;
            let mut e = backends::make(&args[5], cols, rows, 1000);
            e.feed(&bin);
            let c = e.canon();
            println!("{}", c.cursor_col);
        }
        "unvis" => {
            let s = std::fs::read_to_string(&args[2])?;
            std::io::Write::write_all(&mut std::io::stdout(), &capture::unvis(s.trim_end().as_bytes()))?;
        }
        _ => bail!("usage: capture-pty | capture-tmux | snapshot | bench-rss | bench-cpu | unvis"),
    }
    Ok(())
}
