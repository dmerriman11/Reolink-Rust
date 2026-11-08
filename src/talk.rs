//! Talking through a camera's speaker: the page's microphone samples become the DVI-4 ADPCM blocks the camera plays.

use crossbeam_channel::{Sender, TrySendError};

const STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66, 73, 80, 88, 97, 107,
    118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449, 494, 544, 598, 658, 724, 796, 876, 963,
    1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272, 2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894,
    6484, 7132, 7845, 8630, 9493, 10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794,
    32767,
];
const INDEX_CHANGE: [i32; 8] = [-1, -1, -1, -1, 2, 4, 6, 8];

/// One sample's 4-bit code, moving the predictor and step index the way the camera's decoder will.
fn encode_sample(sample: i16, predictor: &mut i32, index: &mut usize) -> u8 {
    let step = STEPS[*index];
    let diff = sample as i32 - *predictor;
    let mut code = if diff < 0 { 8 } else { 0 };
    let (mut left, mut part, mut delta) = (diff.abs(), step, step >> 3);
    for bit in [4, 2, 1] {
        if left >= part {
            code |= bit;
            left -= part;
            delta += part;
        }
        part >>= 1;
    }
    *predictor = (*predictor + if diff < 0 { -delta } else { delta }).clamp(-32768, 32767);
    *index = (*index as i32 + INDEX_CHANGE[(code & 7) as usize]).clamp(0, 88) as usize;
    code
}

/// A block: the first sample and step index as a 4-byte header, then two codes per byte, low nibble first. The header
/// restarts the decoder, so a block dropped in between costs only its own audio.
fn encode_block(samples: &[i16], index: &mut usize) -> Vec<u8> {
    let mut predictor = samples[0] as i32;
    let mut block = Vec::with_capacity(4 + samples.len() / 2);
    block.extend_from_slice(&samples[0].to_le_bytes());
    block.extend_from_slice(&[*index as u8, 0]);
    for pair in samples[1..].chunks_exact(2) {
        let low = encode_sample(pair[0], &mut predictor, index);
        block.push(low | encode_sample(pair[1], &mut predictor, index) << 4);
    }
    block
}

/// A talk in progress. Dropping it ends the talk: the camera's sender sees the channel close and tells the camera.
pub struct Session {
    tx: Sender<Vec<u8>>,
    pending: Vec<i16>,
    /// Header sample plus two per data byte.
    per_block: usize,
    index: usize,
    pub rate: u32,
}

impl Session {
    pub fn new(tx: Sender<Vec<u8>>, block_size: usize, rate: u32) -> Self {
        Self { tx, pending: vec![], per_block: block_size * 2 + 1, index: 0, rate }
    }

    /// Errors once the camera's side has ended, as a full queue only drops audio to keep the delay short.
    pub fn push(&mut self, samples: &[i16]) -> Result<(), String> {
        self.pending.extend_from_slice(samples);
        let whole = self.pending.len() / self.per_block * self.per_block;
        for samples in self.pending[..whole].chunks_exact(self.per_block) {
            match self.tx.try_send(encode_block(samples, &mut self.index)) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => return Err("The camera stopped listening".into()),
            }
        }
        self.pending.drain(..whole);
        Ok(())
    }
}

/// Decode one camera IMA block; each block carries its own predictor.
pub fn decode(block: &[u8]) -> Option<Vec<i16>> {
    if block.len() < 4 || block[2] > 88 || block[3] != 0 { return None; }
    let mut predictor = i16::from_le_bytes([block[0], block[1]]) as i32;
    let mut index = block[2] as i32;
    let mut out = vec![predictor as i16];
    for code in block[4..].iter().flat_map(|b| [b & 15, b >> 4]) {
        let step = STEPS[index as usize];
        let mut delta = step >> 3;
        if code & 4 != 0 { delta += step; }
        if code & 2 != 0 { delta += step >> 1; }
        if code & 1 != 0 { delta += step >> 2; }
        predictor = (predictor + if code & 8 != 0 { -delta } else { delta }).clamp(-32768, 32767);
        index = (index + INDEX_CHANGE[(code & 7) as usize]).clamp(0, 88);
        out.push(predictor as i16);
    }
    Some(out)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decoding_validates_headers_and_nibble_order() {
        assert_eq!(decode(&[0, 0, 0, 0, 0x71]), Some(vec![0, 1, 12]));
        assert_eq!(decode(&[0xff, 0x7f, 88, 0, 0x77]), Some(vec![32767; 3]));
        assert_eq!(decode(&[0, 0]), None);
        assert_eq!(decode(&[0, 0, 89, 0]), None);
        assert_eq!(decode(&[0, 0, 0, 1]), None);
    }

    fn tone(n: usize) -> Vec<i16> {
        (0..n).map(|i| ((i as f64 * 0.05).sin() * 12000.0) as i16).collect()
    }

    #[test]
    fn decoding_a_block_gives_back_the_sound() {
        let input = tone(1025);
        let mut index = 0;
        let block = encode_block(&input, &mut index);
        assert_eq!(block.len(), 4 + 512);
        let output = decode(&block).unwrap();
        assert_eq!(output.len(), input.len());
        // The step size needs a moment to grow to the signal.
        for (a, b) in input.iter().zip(&output).skip(100) {
            assert!((*a as i32 - *b as i32).abs() < 600, "{a} vs {b}");
        }
    }

    #[test]
    fn samples_wait_for_a_whole_block_and_the_step_carries_on() {
        let (tx, rx) = crossbeam_channel::bounded(8);
        let mut s = Session::new(tx, 512, 16000);
        s.push(&tone(1000)).unwrap();
        assert!(rx.try_recv().is_err());
        s.push(&tone(1100)).unwrap();
        let (first, second) = (rx.try_recv().unwrap(), rx.try_recv().unwrap());
        assert!(rx.try_recv().is_err());
        assert_eq!((first.len(), second.len()), (516, 516));
        assert_eq!(first[2], 0);
        assert!(second[2] > 0);
        assert_eq!(s.pending.len(), 50);
    }

    #[test]
    fn a_full_queue_drops_audio_and_a_closed_one_ends_the_talk() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let mut s = Session::new(tx, 512, 16000);
        s.push(&tone(1025 * 3)).unwrap();
        assert_eq!(rx.len(), 1);
        drop(rx);
        assert!(s.push(&tone(1025)).is_err());
    }
}
