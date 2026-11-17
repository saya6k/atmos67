//! Room correction in the AES67 sender (tasks/dsp.md): the filters of a
//! CamillaDSP configuration as SECS 2R writes it, run in f64 on blocks of
//! 48 kHz samples, one channel at a time.

pub mod biquad;
pub mod camilla;
pub mod conv;
pub mod design;
#[cfg(test)]
mod secs_parity;

use std::collections::VecDeque;

/// A whole-sample delay.
pub struct Delay {
    line: VecDeque<f64>,
}

impl Delay {
    pub fn new(samples: usize) -> Self {
        Delay { line: VecDeque::from(vec![0.0; samples]) }
    }

    pub fn process(&mut self, block: &mut [f64]) {
        if self.line.is_empty() {
            return;
        }
        for sample in block {
            self.line.push_back(*sample);
            *sample = self.line.pop_front().expect("the line is never empty");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_shifts_across_blocks() {
        let mut delay = Delay::new(3);
        let mut first = [1.0, 2.0];
        let mut second = [3.0, 4.0, 5.0, 6.0];
        delay.process(&mut first);
        delay.process(&mut second);
        assert_eq!(first, [0.0, 0.0]);
        assert_eq!(second, [0.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn zero_delay_passes_through() {
        let mut block = [1.0, 2.0];
        Delay::new(0).process(&mut block);
        assert_eq!(block, [1.0, 2.0]);
    }
}
