// SPDX-License-Identifier: MIT OR Apache-2.0

//! Sound: FTE's `queueaudio`/`getqueuedaudiotime` builtins feeding a cpal output stream.
//!
//! The progs mixes into its own ring buffer and queues what it painted; the host plays the queue
//! and reports how much of it is still waiting, from which Quake derives its play cursor.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, FromSample, SampleFormat, SizedSample, Stream, StreamConfig};
use qcvm::{Vm, VmError};

use crate::host::MenuHost;

type B = Result<(), VmError>;

/// Memory cap: older queued audio is dropped beyond this. Quake never queues more than its
/// 16384-frame ring (1.5 s at 11025 Hz); dropping throws its play cursor off, so this only stops
/// a runaway progs.
const MAX_QUEUED_SECS: u32 = 4;
/// Largest single submission accepted, in frames.
const MAX_FRAMES: u32 = 1 << 20;

/// Queued audio, interleaved at the progs' rate and channel count.
#[derive(Default)]
struct Queue {
    rate: u32,
    channels: usize,
    samples: VecDeque<f32>,
    /// Position between the first and second queued frame (linear interpolation).
    frac: f64,
}

impl Queue {
    fn frames(&self) -> usize {
        self.samples.len().checked_div(self.channels).unwrap_or(0)
    }

    fn push(&mut self, rate: u32, channels: usize, samples: impl Iterator<Item = f32>) {
        if rate != self.rate || channels != self.channels {
            *self = Self { rate, channels, ..Self::default() };
        }
        self.samples.extend(samples);
        let max = usize::try_from(rate.saturating_mul(MAX_QUEUED_SECS)).unwrap_or(usize::MAX);
        let excess = self.frames().saturating_sub(max).saturating_mul(channels);
        self.samples.drain(..excess);
    }

    /// Fills `out` (interleaved, `out_channels` wide, at `out_rate`), resampling linearly.
    fn play<T: SizedSample + FromSample<f32>>(
        &mut self,
        out: &mut [T],
        out_channels: usize,
        out_rate: u32,
    ) {
        let step = f64::from(self.rate) / f64::from(out_rate);
        let ch = self.channels;
        for frame in out.chunks_mut(out_channels) {
            if self.frames() == 0 {
                frame.fill(T::EQUILIBRIUM);
                self.frac = 0.0;
                continue;
            }
            for (i, s) in frame.iter_mut().enumerate() {
                let c = i.checked_rem(ch).unwrap_or(0);
                let a = self.samples.get(c).copied().unwrap_or(0.0);
                let b = self.samples.get(ch.saturating_add(c)).copied().unwrap_or(a);
                *s = T::from_sample(a + (b - a) * self.frac as f32);
            }
            self.frac += step;
            while self.frac >= 1.0 && self.frames() > 0 {
                self.samples.drain(..ch);
                self.frac -= 1.0;
            }
        }
    }
}

pub struct Audio {
    queue: Arc<Mutex<Queue>>,
    _stream: Stream,
}

impl Audio {
    /// Opens the default output device, or explains on stderr why there is no sound.
    pub fn open() -> Option<Self> {
        let device = cpal::default_host().default_output_device();
        let Some(device) = device else {
            eprintln!("audio: no output device");
            return None;
        };
        let config = match device.default_output_config() {
            Ok(c) => c,
            Err(e) => {
                eprintln!("audio: {e}");
                return None;
            }
        };
        let queue = Arc::new(Mutex::new(Queue::default()));
        let q = Arc::clone(&queue);
        let stream = match config.sample_format() {
            SampleFormat::F32 => build::<f32>(&device, config.into(), q),
            SampleFormat::I16 => build::<i16>(&device, config.into(), q),
            SampleFormat::U16 => build::<u16>(&device, config.into(), q),
            SampleFormat::I32 => build::<i32>(&device, config.into(), q),
            other => {
                eprintln!("audio: unsupported sample format {other}");
                return None;
            }
        };
        match stream.and_then(|s| s.play().map(|()| s)) {
            Ok(stream) => Some(Self { queue, _stream: stream }),
            Err(e) => {
                eprintln!("audio: {e}");
                None
            }
        }
    }

    fn queue(&self) -> MutexGuard<'_, Queue> {
        self.queue.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn build<T: SizedSample + FromSample<f32>>(
    device: &Device,
    config: StreamConfig,
    queue: Arc<Mutex<Queue>>,
) -> Result<Stream, cpal::Error> {
    let (channels, rate) = (usize::from(config.channels), config.sample_rate);
    device.build_output_stream(
        config,
        move |out: &mut [T], _| {
            queue.lock().unwrap_or_else(PoisonError::into_inner).play(out, channels, rate);
        },
        |e| eprintln!("audio: {e}"),
        None,
    )
}

/// `float queueaudio(int hz, int channels, int type, void *data, unsigned int frames)`: queues
/// `frames` frames of unsigned 8-bit (`type` 8) or signed 16-bit (`type` -16) samples.
pub fn queueaudio(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    let (hz, channels, kind) = (vm.arg_i32(0), vm.arg_i32(1), vm.arg_i32(2));
    let frames = vm.arg_u32(4).min(MAX_FRAMES);
    let width = match kind {
        8 => 1,
        -16 => 2,
        _ => {
            vm.warn(format!("queueaudio: unsupported sample type {kind}"));
            vm.ret_f32(0.0);
            return Ok(());
        }
    };
    let (Ok(rate @ 1..), Ok(ch @ 1..=8)) = (u32::try_from(hz), usize::try_from(channels)) else {
        vm.warn(format!("queueaudio: bad format {hz} Hz, {channels} channels"));
        vm.ret_f32(0.0);
        return Ok(());
    };
    let len = usize::try_from(frames).unwrap_or(0).saturating_mul(ch).saturating_mul(width);
    let (Some(audio), Some(bytes)) = (&h.audio, vm.read_mem(vm.arg_ptr(3), len)) else {
        vm.ret_f32(0.0);
        return Ok(());
    };
    let mut queue = audio.queue();
    if width == 1 {
        queue.push(rate, ch, bytes.iter().map(|&b| (f32::from(b) - 128.0) / 128.0));
    } else {
        let samples = bytes.chunks_exact(2).map(|s| {
            let [lo, hi] = [s.first().copied().unwrap_or(0), s.get(1).copied().unwrap_or(0)];
            f32::from(i16::from_le_bytes([lo, hi])) / 32768.0
        });
        queue.push(rate, ch, samples);
    }
    vm.ret_f32(1.0);
    Ok(())
}

/// `float getqueuedaudiotime()`: seconds of queued audio not yet played.
///
/// Quake turns this back into whole frames (`secs * rate`, truncated) and treats its play cursor
/// moving backwards as a wrap of its ring buffer, jumping 16384 frames ahead. So the value is
/// whole frames plus half a frame: truncation then gives exactly the queued frame count, and the
/// cursor never steps back through rounding.
pub fn getqueuedaudiotime(vm: &mut Vm<MenuHost>, h: &mut MenuHost) -> B {
    let secs = h.audio.as_ref().map_or(0.0, |a| {
        let q = a.queue();
        if q.rate == 0 { 0.0 } else { (q.frames() as f64 + 0.5) / f64::from(q.rate) }
    });
    vm.ret_f32(secs as f32);
    Ok(())
}
