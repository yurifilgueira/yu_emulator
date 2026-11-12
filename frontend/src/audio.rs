//! Sound output: plays the samples a core produces on the default device.
//!
//! The core produces stereo samples at its own rate (44100 Hz for the
//! PlayStation); the device asks for samples at its rate, from its own
//! thread. In between sits a queue and a linear-interpolation resampler.
//! When the emulator runs slower than real time the queue runs dry and the
//! last sample is held (audible as crackling); when it runs faster, the
//! oldest samples are dropped.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// Samples waiting to be played, and the resampling position.
pub struct Queue {
    samples: VecDeque<[i16; 2]>,
    /// Source samples per device sample.
    step: f64,
    /// Position between `previous` (0.0) and the front of the queue (1.0).
    position: f64,
    previous: [i16; 2],
    /// Most samples kept; more than that means the emulator is ahead.
    capacity: usize,
}

impl Queue {
    pub fn new(source_rate: u32, device_rate: u32) -> Self {
        Self {
            samples: VecDeque::new(),
            step: source_rate as f64 / device_rate as f64,
            position: 0.0,
            previous: [0; 2],
            // A fifth of a second: enough to ride out uneven frame times
            // without a noticeable delay.
            capacity: source_rate as usize / 5,
        }
    }

    pub fn push(&mut self, samples: &[[i16; 2]]) {
        self.samples.extend(samples);
        let excess = self.samples.len().saturating_sub(self.capacity);
        self.samples.drain(..excess);
    }

    /// The next stereo sample at the device's rate, in -1.0..1.0.
    pub fn next(&mut self) -> [f32; 2] {
        while self.position >= 1.0 {
            match self.samples.pop_front() {
                Some(sample) => {
                    self.previous = sample;
                    self.position -= 1.0;
                }
                // Nothing to play: hold the last sample.
                None => {
                    self.position = 1.0;
                    return self.previous.map(|v| v as f32 / 32768.0);
                }
            }
        }
        let next = self.samples.front().copied().unwrap_or(self.previous);
        let t = self.position as f32;
        self.position += self.step;
        std::array::from_fn(|side| {
            let (a, b) = (self.previous[side] as f32, next[side] as f32);
            (a + (b - a) * t) / 32768.0
        })
    }
}

/// Writes interleaved frames of `channels` samples: stereo goes to the
/// first two channels, mono gets the average, further channels silence.
fn fill<T: Copy>(queue: &Mutex<Queue>, data: &mut [T], channels: usize, convert: impl Fn(f32) -> T) {
    let mut queue = queue.lock().unwrap_or_else(|e| e.into_inner());
    for frame in data.chunks_mut(channels) {
        let [left, right] = queue.next();
        for (channel, out) in frame.iter_mut().enumerate() {
            *out = convert(match (channels, channel) {
                (1, _) => (left + right) / 2.0,
                (_, 0) => left,
                (_, 1) => right,
                _ => 0.0,
            });
        }
    }
}

/// An open output device. Dropping it stops the sound.
pub struct Output {
    queue: Arc<Mutex<Queue>>,
    _stream: cpal::Stream,
}

impl Output {
    /// Opens the default output device for a source of `source_rate` Hz.
    pub fn open(source_rate: u32) -> Result<Self, String> {
        let device = cpal::default_host().default_output_device().ok_or("no sound output device")?;
        let supported = device.default_output_config().map_err(|e| e.to_string())?;
        let config: cpal::StreamConfig = supported.config();
        let channels = config.channels as usize;
        let queue = Arc::new(Mutex::new(Queue::new(source_rate, config.sample_rate.0)));
        let on_error = |e| eprintln!("sound output error: {e}");
        let q = queue.clone();
        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => {
                device.build_output_stream(&config, move |data: &mut [f32], _: &_| fill(&q, data, channels, |v| v), on_error, None)
            }
            cpal::SampleFormat::I16 => device.build_output_stream(
                &config,
                move |data: &mut [i16], _: &_| fill(&q, data, channels, |v| (v * 32767.0) as i16),
                on_error,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_output_stream(
                &config,
                move |data: &mut [u16], _: &_| fill(&q, data, channels, |v| ((v + 1.0) * 32767.5) as u16),
                on_error,
                None,
            ),
            other => return Err(format!("unsupported sound sample format {other:?}")),
        }
        .map_err(|e| e.to_string())?;
        stream.play().map_err(|e| e.to_string())?;
        Ok(Self { queue, _stream: stream })
    }

    /// Hands the device the samples produced since the last call.
    pub fn push(&self, samples: &[[i16; 2]]) {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).push(samples);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_rate_passes_samples_through() {
        let mut queue = Queue::new(44_100, 44_100);
        queue.push(&[[16384, -16384], [8192, 0]]);
        // The first output is the (silent) starting point, then the samples.
        assert_eq!(queue.next(), [0.0, 0.0]);
        assert_eq!(queue.next(), [0.5, -0.5]);
        assert_eq!(queue.next(), [0.25, 0.0]);
    }

    #[test]
    fn upsampling_interpolates_between_samples() {
        // Twice the device rate: every source sample and the midpoints.
        let mut queue = Queue::new(22_050, 44_100);
        queue.push(&[[16384, 0], [0, 16384]]);
        let left: Vec<f32> = (0..5).map(|_| queue.next()[0]).collect();
        assert_eq!(left, [0.0, 0.25, 0.5, 0.25, 0.0]);
    }

    #[test]
    fn an_empty_queue_holds_the_last_sample() {
        let mut queue = Queue::new(44_100, 44_100);
        queue.push(&[[16384, 16384]]);
        queue.next();
        assert_eq!(queue.next(), [0.5, 0.5]);
        assert_eq!(queue.next(), [0.5, 0.5]);
        assert_eq!(queue.next(), [0.5, 0.5]);
        // A new sample is played as soon as it arrives.
        queue.push(&[[0, 0]]);
        assert_eq!(queue.next(), [0.0, 0.0]);
    }

    #[test]
    fn a_full_queue_drops_the_oldest_samples() {
        let mut queue = Queue::new(1000, 1000); // capacity 200
        let samples: Vec<[i16; 2]> = (0..300).map(|i| [i, i]).collect();
        queue.push(&samples);
        assert_eq!(queue.samples.len(), 200);
        assert_eq!(queue.samples[0], [100, 100]);
    }

    #[test]
    fn frames_are_laid_out_per_channel_count() {
        let queue = Mutex::new(Queue::new(44_100, 44_100));
        queue.lock().unwrap().push(&[[16384, 0], [16384, 0], [16384, 0]]);
        queue.lock().unwrap().next();
        let mut mono = [9.0f32; 1];
        fill(&queue, &mut mono, 1, |v| v);
        assert_eq!(mono, [0.25]);
        let mut surround = [9.0f32; 4];
        fill(&queue, &mut surround, 4, |v| v);
        assert_eq!(surround, [0.5, 0.0, 0.0, 0.0]);
    }
}
