//! Video decode and MP4 recording via an ffmpeg subprocess.
//! shortcut: needs ffmpeg.exe on PATH or next to the exe; upgrade to linked libavcodec if that's a burden.

use neolink_core::bcmedia::model::VideoType;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{SyncSender, TrySendError, sync_channel};

/// Largest decoded width; 4K RGBA at full size would push ~600 MB/s through the pipe.
const MAX_WIDTH: u32 = 1920;


fn ffmpeg() -> PathBuf {
    let local = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("ffmpeg.exe")));
    match local {
        Some(p) if p.exists() => p,
        _ => PathBuf::from("ffmpeg"),
    }
}

fn format_of(codec: VideoType) -> &'static str {
    match codec {
        VideoType::H264 => "h264",
        VideoType::H265 => "hevc",
    }
}

fn spawn(args: &[&str], stdout: bool) -> std::io::Result<Child> {
    let mut cmd = Command::new(ffmpeg());
    cmd.args(["-hide_banner", "-loglevel", "error"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(if stdout { Stdio::piped() } else { Stdio::null() })
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    cmd.spawn()
}

/// Feeds Annex-B NAL units to a writer thread so a slow ffmpeg never stalls the network task.
fn feeder(mut stdin: ChildStdin) -> SyncSender<Vec<u8>> {
    let (tx, rx) = sync_channel::<Vec<u8>>(64);
    std::thread::spawn(move || {
        for buf in rx {
            if stdin.write_all(&buf).is_err() {
                break;
            }
        }
    });
    tx
}

pub struct Decoder {
    pub codec: VideoType,
    child: Child,
    tx: Option<SyncSender<Vec<u8>>>,
    waiting_for_key: bool,
}

impl Decoder {
    pub fn new(
        codec: VideoType,
        on_frame: impl Fn(Vec<u8>) + Send + 'static,
    ) -> std::io::Result<Self> {
        // JPEG frames carry their own size, so we never need the camera to tell us the resolution
        // (some, like the Argus 4 Pro, never send the stream info header).
        let scale = format!("scale='min({MAX_WIDTH},iw)':-2");
        let mut child = spawn(
            &[
                "-flags", "low_delay", "-probesize", "32", "-analyzeduration", "0",
                "-f", format_of(codec), "-i", "pipe:0",
                "-vf", &scale, "-c:v", "mjpeg", "-q:v", "4", "-f", "mpjpeg", "pipe:1",
            ],
            true,
        )?;
        let tx = feeder(child.stdin.take().expect("piped stdin"));
        let mut stdout = std::io::BufReader::new(child.stdout.take().expect("piped stdout"));
        std::thread::spawn(move || {
            while let Some(frame) = read_mpjpeg(&mut stdout) {
                on_frame(frame);
            }
        });
        Ok(Self { codec, child, tx: Some(tx), waiting_for_key: true })
    }

    pub fn push(&mut self, data: Vec<u8>, keyframe: bool) {
        if self.waiting_for_key && !keyframe {
            return;
        }
        self.waiting_for_key = false;
        if let Some(tx) = &self.tx
            && let Err(TrySendError::Full(_)) = tx.try_send(data)
        {
            // Dropped a frame; resync on the next keyframe instead of decoding garbage.
            self.waiting_for_key = true;
        }
    }
}

impl Drop for Decoder {
    fn drop(&mut self) {
        self.tx.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Read one part of ffmpeg's mpjpeg output: "--ffmpeg", headers incl. Content-length, blank line, JPEG.
fn read_mpjpeg(r: &mut impl std::io::BufRead) -> Option<Vec<u8>> {
    let mut len = None;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let l = line.trim_end();
        if let Some((k, v)) = l.split_once(':')
            && k.eq_ignore_ascii_case("content-length")
        {
            len = v.trim().parse::<usize>().ok();
        } else if l.is_empty() && len.is_some() {
            break;
        }
    }
    let mut jpeg = vec![0u8; len?];
    r.read_exact(&mut jpeg).ok()?;
    Some(jpeg)
}

pub struct Recorder {
    pub path: PathBuf,
    child: Option<Child>,
    tx: Option<SyncSender<Vec<u8>>>,
    started: bool,
}

impl Recorder {
    pub fn new(codec: VideoType, path: &Path) -> std::io::Result<Self> {
        let out = path.to_string_lossy().into_owned();
        let mut child = spawn(
            &[
                "-use_wallclock_as_timestamps", "1", "-f", format_of(codec), "-i", "pipe:0",
                "-c", "copy", "-movflags", "+faststart", "-y", &out,
            ],
            false,
        )?;
        let tx = feeder(child.stdin.take().expect("piped stdin"));
        Ok(Self { path: path.to_owned(), child: Some(child), tx: Some(tx), started: false })
    }

    pub fn push(&mut self, data: &[u8], keyframe: bool) {
        self.started |= keyframe;
        if self.started && let Some(tx) = &self.tx {
            // Blocking send: a recording must not silently lose frames.
            let _ = tx.send(data.to_vec());
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Closing stdin lets ffmpeg write the moov atom; wait off-thread so the UI never blocks.
        self.tx.take();
        if let Some(mut child) = self.child.take() {
            std::thread::spawn(move || child.wait());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Encode a test pattern with ffmpeg and check the decoder hands back frames of the scaled size.
    fn roundtrip(codec: VideoType, enc: &str) {
        let out = Command::new(ffmpeg())
            .args(["-hide_banner", "-loglevel", "error", "-f", "lavfi", "-i", "testsrc=size=2560x1440:rate=10", "-frames:v", "10", "-c:v", enc, "-f", format_of(codec), "pipe:1"])
            .output()
            .expect("ffmpeg on PATH");
        assert!(out.status.success() && !out.stdout.is_empty());
        let (tx, rx) = std::sync::mpsc::channel();
        let mut dec = Decoder::new(codec, move |jpeg| { let _ = tx.send(jpeg); }).unwrap();
        dec.push(out.stdout, true);
        dec.tx.take(); // EOF flushes the decoder
        let jpeg = rx.recv_timeout(std::time::Duration::from_secs(20)).expect("a decoded frame");
        assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "JPEG SOI");
        // SOF0 marker carries the scaled size: 2560x1440 capped to 1920 wide.
        let sof = jpeg.windows(2).position(|w| w == [0xFF, 0xC0]).expect("SOF0");
        let h = u16::from_be_bytes([jpeg[sof + 5], jpeg[sof + 6]]);
        let w = u16::from_be_bytes([jpeg[sof + 7], jpeg[sof + 8]]);
        assert_eq!((w, h), (1920, 1080));
    }

    #[test]
    fn decodes_h264() {
        roundtrip(VideoType::H264, "libx264");
    }

    #[test]
    fn decodes_h265() {
        roundtrip(VideoType::H265, "libx265");
    }

    #[test]
    fn parses_mpjpeg() {
        let data = b"--ffmpeg\r\nContent-type: image/jpeg\r\nContent-length: 3\r\n\r\nabc\r\n--ffmpeg\r\nContent-length: 2\r\n\r\nxy\r\n";
        let mut r = &data[..];
        assert_eq!(read_mpjpeg(&mut r).unwrap(), b"abc");
        assert_eq!(read_mpjpeg(&mut r).unwrap(), b"xy");
        assert!(read_mpjpeg(&mut r).is_none());
    }
}
