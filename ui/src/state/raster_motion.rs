use image::{AnimationDecoder, ImageDecoder, ImageFormat, Limits, metadata::LoopCount};
use rune::{Pixels, Tx};
use std::{io::Cursor, time::Duration};

pub struct Motion {
    frames: Vec<(Pixels, Duration)>,
    frame: usize,
    remaining: Option<u32>,
    next: Option<f64>,
    finished: bool,
}

impl Motion {
    pub fn open(
        data: &[u8],
        format: ImageFormat,
        limits: Limits,
        size: (u32, u32),
    ) -> Option<Self> {
        let bytes_per_frame = u64::from(size.0) * u64::from(size.1) * 4;
        let max = (24 * 1024 * 1024 / bytes_per_frame).min(128) as usize;
        if max < 2 {
            return None;
        }
        let (count, frames) = match format {
            ImageFormat::Gif => {
                let mut d = image::codecs::gif::GifDecoder::new(Cursor::new(data)).ok()?;
                d.set_limits(limits).ok()?;
                (d.loop_count(), d.into_frames())
            }
            ImageFormat::WebP => {
                let mut d = image::codecs::webp::WebPDecoder::new(Cursor::new(data)).ok()?;
                if !d.has_animation() {
                    return None;
                }
                d.set_limits(limits).ok()?;
                (d.loop_count(), d.into_frames())
            }
            _ => return None,
        };
        let mut out = Vec::new();
        for frame in frames {
            if out.len() == max {
                return None;
            }
            let frame = frame.ok()?;
            let delay: Duration = frame.delay().into();
            let image = frame.into_buffer();
            if image.dimensions() != size {
                return None;
            }
            let pixels = Pixels::new();
            pixels.set(size.0, size.1, image.into_raw());
            out.push((pixels, delay.max(Duration::from_millis(40))));
        }
        (out.len() > 1).then(|| Self {
            frames: out,
            frame: 0,
            next: None,
            finished: false,
            remaining: match count {
                LoopCount::Infinite => None,
                LoopCount::Finite(n) => Some(n.get()),
            },
        })
    }
    pub fn pixels(&self) -> &Pixels {
        &self.frames[self.frame].0
    }
    pub fn poll(&mut self, now: f64, tx: &mut Tx) -> bool {
        if self.finished {
            return false;
        }
        let next = *self
            .next
            .get_or_insert_with(|| now + self.frames[self.frame].1.as_secs_f64());
        if now < next {
            tx.wake_after(Duration::from_secs_f64((next - now).min(60.0)));
            return false;
        }
        if self.frame + 1 == self.frames.len() {
            if self.remaining == Some(1) {
                self.finished = true;
                return false;
            }
            if let Some(n) = &mut self.remaining {
                *n -= 1;
            }
        }
        self.frame = (self.frame + 1) % self.frames.len();
        let delay = self.frames[self.frame].1;
        self.next = Some(now + delay.as_secs_f64());
        tx.wake_after(delay.min(Duration::from_secs(60)));
        true
    }
}
