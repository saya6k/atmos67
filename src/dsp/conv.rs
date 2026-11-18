//! FIR convolution by uniformly partitioned overlap-save: the impulse
//! response is cut into block-sized partitions, each transformed once, and
//! every block of input is transformed once and multiplied against all of
//! them through a delay line of past input spectra.

use std::sync::Arc;

use realfft::num_complex::Complex;
use realfft::{ComplexToReal, RealFftPlanner, RealToComplex};

pub struct Convolver {
    block: usize,
    forward: Arc<dyn RealToComplex<f64>>,
    inverse: Arc<dyn ComplexToReal<f64>>,
    /// Spectrum of each partition, scaled for the unnormalized inverse;
    /// shared by the convolvers of one filter.
    partitions: Arc<Vec<Vec<Complex<f64>>>>,
    /// Spectra of the latest inputs; `spectra[newest]` is the current one.
    spectra: Vec<Vec<Complex<f64>>>,
    newest: usize,
    /// The previous block followed by the current one.
    window: Vec<f64>,
    time: Vec<f64>,
    sum: Vec<Complex<f64>>,
}

impl Convolver {
    /// A convolver with `taps` that processes `block` samples at a time.
    pub fn new(taps: &[f64], block: usize) -> Self {
        let mut planner = RealFftPlanner::new();
        let forward = planner.plan_fft_forward(2 * block);
        let inverse = planner.plan_fft_inverse(2 * block);
        let scale = 1.0 / (2 * block) as f64;
        let partitions: Vec<_> = taps
            .chunks(block)
            .map(|part| {
                let mut time = vec![0.0; 2 * block];
                for (slot, tap) in time.iter_mut().zip(part) {
                    *slot = tap * scale;
                }
                let mut spectrum = forward.make_output_vec();
                forward.process(&mut time, &mut spectrum).expect("buffer sizes come from the plan");
                spectrum
            })
            .collect();
        Self::with(forward, inverse, Arc::new(partitions), block)
    }

    fn with(
        forward: Arc<dyn RealToComplex<f64>>,
        inverse: Arc<dyn ComplexToReal<f64>>,
        partitions: Arc<Vec<Vec<Complex<f64>>>>,
        block: usize,
    ) -> Self {
        Convolver {
            block,
            sum: forward.make_output_vec(),
            spectra: vec![forward.make_output_vec(); partitions.len().max(1)],
            forward,
            inverse,
            partitions,
            newest: 0,
            window: vec![0.0; 2 * block],
            time: vec![0.0; 2 * block],
        }
    }

    /// A convolver with the same taps and its own state, without
    /// transforming the taps again.
    pub fn sibling(&self) -> Self {
        Self::with(Arc::clone(&self.forward), Arc::clone(&self.inverse), Arc::clone(&self.partitions), self.block)
    }

    /// Convolves one block of exactly `block` samples in place.
    pub fn process(&mut self, block: &mut [f64]) {
        let n = self.block;
        assert_eq!(block.len(), n, "convolver block size");
        self.window.copy_within(n.., 0);
        self.window[n..].copy_from_slice(block);

        self.newest = (self.newest + 1) % self.spectra.len();
        self.time.copy_from_slice(&self.window);
        self.forward
            .process(&mut self.time, &mut self.spectra[self.newest])
            .expect("buffer sizes come from the plan");

        self.sum.fill(Complex::default());
        let count = self.spectra.len();
        for (age, partition) in self.partitions.iter().enumerate() {
            let input = &self.spectra[(self.newest + count - age) % count];
            for ((sum, x), h) in self.sum.iter_mut().zip(input).zip(partition) {
                *sum += x * h;
            }
        }
        // Rounding leaves a trace of imaginary part at DC and Nyquist, which
        // the inverse transform refuses.
        self.sum[0].im = 0.0;
        self.sum[n].im = 0.0;
        self.inverse
            .process(&mut self.sum, &mut self.time)
            .expect("buffer sizes come from the plan");
        block.copy_from_slice(&self.time[n..]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Deterministic values in [-1, 1).
    fn noise(len: usize, seed: u64) -> Vec<f64> {
        let mut state = seed;
        (0..len)
            .map(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                (state >> 11) as f64 / (1u64 << 52) as f64 - 1.0
            })
            .collect()
    }

    fn direct(input: &[f64], taps: &[f64]) -> Vec<f64> {
        (0..input.len())
            .map(|n| taps.iter().enumerate().filter(|(k, _)| *k <= n).map(|(k, h)| h * input[n - k]).sum())
            .collect()
    }

    fn check(taps_len: usize, block: usize) {
        let taps = noise(taps_len, 1);
        let input = noise(block * 12, 2);
        let mut convolver = Convolver::new(&taps, block);
        let mut output = input.clone();
        for chunk in output.chunks_mut(block) {
            convolver.process(chunk);
        }
        let expected = direct(&input, &taps);
        let worst = output.iter().zip(&expected).map(|(a, b)| (a - b).abs()).fold(0.0, f64::max);
        assert!(worst < 1e-9, "{taps_len} taps, block {block}: {worst}");
    }

    #[test]
    fn matches_direct_convolution() {
        check(1, 64);
        check(50, 64);
        check(64, 64);
        check(700, 64);
        check(1000, 128);
    }

    #[test]
    fn a_sibling_convolves_alike_from_its_own_state() {
        let taps = noise(300, 4);
        let input = noise(64 * 8, 5);
        let mut first = Convolver::new(&taps, 64);
        let mut warm = input.clone();
        warm.chunks_mut(64).for_each(|chunk| first.process(chunk));
        let mut second = first.sibling();
        let mut fresh = Convolver::new(&taps, 64);
        let (mut a, mut b) = (input.clone(), input);
        a.chunks_mut(64).for_each(|chunk| second.process(chunk));
        b.chunks_mut(64).for_each(|chunk| fresh.process(chunk));
        assert_eq!(a, b);
    }

    #[test]
    fn a_unit_impulse_passes_the_input_through() {
        let input = noise(256, 3);
        let mut convolver = Convolver::new(&[1.0], 64);
        let mut output = input.clone();
        for chunk in output.chunks_mut(64) {
            convolver.process(chunk);
        }
        for (a, b) in output.iter().zip(&input) {
            assert!((a - b).abs() < 1e-12);
        }
    }
}
