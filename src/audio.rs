//! Bounded live audio fan-out. Decoding belongs to the HTTP consumer, never the camera task.
use neolink_core::bcmedia::model::BcMedia;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Format {
    pub codec: &'static str,
    pub rate: u32,
    pub channels: u8,
}

pub struct Frame {
    pub format: Format,
    pub us: u64,
    pub data: Vec<u8>,
}

pub struct Feed {
    format: Mutex<Option<Format>>,
    /// Where the next chunk starts on the camera's clock. Audio carries no stamp of its own and several chunks arrive
    /// per video frame, so each one continues the last, pulled back to the video clock if they drift apart.
    clock: Mutex<Option<u32>>,
    tx: broadcast::Sender<Arc<Frame>>,
}

impl Default for Feed {
    fn default() -> Self {
        Self { format: Mutex::new(None), clock: Mutex::new(None), tx: broadcast::channel(32).0 }
    }
}

impl Feed {
    pub fn clear(&self) {
        *self.format.lock().unwrap() = None;
        *self.clock.lock().unwrap() = None;
    }

    pub fn subscribe(&self) -> (Option<Format>, broadcast::Receiver<Arc<Frame>>) {
        let format = self.format.lock().unwrap();
        (*format, self.tx.subscribe())
    }

    fn send(&self, format: Format, video_us: u32, dur_us: u32, data: Vec<u8>) {
        let us = {
            let mut clock = self.clock.lock().unwrap();
            let us = match *clock {
                Some(next) if (next.wrapping_sub(video_us) as i32).unsigned_abs() < MAX_DRIFT_US => next,
                _ => video_us,
            };
            *clock = Some(us.wrapping_add(dur_us));
            us
        };
        let mut current = self.format.lock().unwrap();
        *current = Some(format);
        let _ = self.tx.send(Arc::new(Frame { format, us: us.into(), data }));
    }

    pub fn push(&self, media: BcMedia, us: u32) {
        match media {
            BcMedia::Adpcm(f) if f.data.len() >= 4 && f.data[2] <= 88 && f.data[3] == 0 => {
                // Incoming BC ADPCM is 8 kHz (BcMediaAdpcm::duration); talk-back has a separate config.
                self.send(Format { codec: "pcm", rate: 8000, channels: 1 }, us, f.duration().unwrap_or(0), f.data);
            }
            BcMedia::Aac(f) => {
                let mut data = f.data.as_slice();
                while let Some((format, len)) = adts(data) {
                    self.send(format, us, 1024 * 1_000_000 / format.rate, data[..len].to_vec()); // an AAC-LC frame
                    data = &data[len..];
                }
            }
            _ => {}
        }
    }
}

const MAX_DRIFT_US: u32 = 500_000;

fn adts(data: &[u8]) -> Option<(Format, usize)> {
    const RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
    if data.len() < 7 || data[0] != 255 || data[1] & 0xf6 != 0xf0 { return None; }
    let rate = *RATES.get(((data[2] >> 2) & 15) as usize)?;
    let channels = ((data[2] & 1) << 2) | (data[3] >> 6);
    let len = (((data[3] & 3) as usize) << 11) | ((data[4] as usize) << 3) | (data[5] as usize >> 5);
    let header = if data[1] & 1 == 0 { 9 } else { 7 };
    if channels != 1 || len < header || len > data.len() { return None; }
    Some((Format { codec: "aac", rate, channels }, len))
}

#[cfg(test)]
mod tests {
    use super::*;
    use neolink_core::bcmedia::model::{BcMediaAac, BcMediaAdpcm};

    #[test]
    fn subscribers_get_format_and_only_live_audio_and_survive_lag() {
        let feed = Feed::default();
        let send = || feed.push(BcMedia::Adpcm(BcMediaAdpcm { data: vec![0; 5] }), 42);
        send();
        let (format, mut rx) = feed.subscribe();
        assert_eq!(format.unwrap().rate, 8000);
        assert!(matches!(rx.try_recv(), Err(broadcast::error::TryRecvError::Empty)));
        for _ in 0..40 { send(); }
        assert!(matches!(rx.try_recv(), Err(broadcast::error::TryRecvError::Lagged(_))));
        assert!(rx.try_recv().unwrap().us >= 42); // chunks run on from the video time
        feed.clear();
        assert!(feed.subscribe().0.is_none());
        drop(feed);
        while rx.try_recv().is_ok() {}
        assert!(matches!(rx.try_recv(), Err(broadcast::error::TryRecvError::Closed)));
    }

    #[test]
    fn chunks_between_video_frames_get_consecutive_times() {
        let feed = Feed::default();
        let (_, mut rx) = feed.subscribe();
        let frame = vec![255, 241, 0x60, 0x40, 0, 0xff, 0xfc]; // mono 16 kHz: 64 ms per frame
        feed.push(BcMedia::Aac(BcMediaAac { data: frame.repeat(3) }), 1_000_000);
        let us: Vec<u64> = (0..3).map(|_| rx.try_recv().unwrap().us).collect();
        assert_eq!(us, [1_000_000, 1_064_000, 1_128_000]);
        feed.push(BcMedia::Aac(BcMediaAac { data: frame.clone() }), 9_000_000); // far from the video clock: resync
        assert_eq!(rx.try_recv().unwrap().us, 9_000_000);
    }

    #[test]
    fn adts_preserves_each_frame_and_reports_rate_changes() {
        let feed = Feed::default();
        let (_, mut rx) = feed.subscribe();
        let frame = vec![255, 241, 0x60, 0x40, 0, 0xff, 0xfc]; // mono 16 kHz, seven bytes
        assert_eq!(adts(&frame).unwrap().0.rate, 16000);
        feed.push(BcMedia::Aac(BcMediaAac { data: frame.repeat(2) }), 123);
        for _ in 0..2 { assert_eq!(rx.try_recv().unwrap().data, frame); }
        let mut changed = frame.clone();
        changed[2] = 0x6c; // 8 kHz
        feed.push(BcMedia::Aac(BcMediaAac { data: changed }), 124);
        assert_eq!(rx.try_recv().unwrap().format.rate, 8000);
        assert!(adts(&frame[..6]).is_none());
        let mut invalid = frame;
        invalid[2] = 0x7c; // reserved rate
        assert!(adts(&invalid).is_none());
    }
}
