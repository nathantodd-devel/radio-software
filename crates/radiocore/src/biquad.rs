//! Second-order IIR sections (RBJ cookbook), transposed direct form II.

use std::f32::consts::PI;

#[derive(Clone, Copy)]
pub struct Biquad {
    b: [f32; 3],
    a: [f32; 2],
    z: [f32; 2],
}

impl Biquad {
    fn new(b: [f32; 3], a: [f32; 3]) -> Self {
        Self {
            b: [b[0] / a[0], b[1] / a[0], b[2] / a[0]],
            a: [a[1] / a[0], a[2] / a[0]],
            z: [0.0; 2],
        }
    }

    pub fn lowpass(fs: f32, fc: f32, q: f32) -> Self {
        let (sin, cos) = (2.0 * PI * fc / fs).sin_cos();
        let alpha = sin / (2.0 * q);
        Self::new(
            [(1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0],
            [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
        )
    }

    pub fn highpass(fs: f32, fc: f32, q: f32) -> Self {
        let (sin, cos) = (2.0 * PI * fc / fs).sin_cos();
        let alpha = sin / (2.0 * q);
        Self::new(
            [(1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0],
            [1.0 + alpha, -2.0 * cos, 1.0 - alpha],
        )
    }

    pub fn notch(fs: f32, fc: f32, q: f32) -> Self {
        let (sin, cos) = (2.0 * PI * fc / fs).sin_cos();
        let alpha = sin / (2.0 * q);
        Self::new([1.0, -2.0 * cos, 1.0], [1.0 + alpha, -2.0 * cos, 1.0 - alpha])
    }

    #[inline]
    pub fn step(&mut self, x: f32) -> f32 {
        let y = self.b[0] * x + self.z[0];
        self.z[0] = self.b[1] * x - self.a[0] * y + self.z[1];
        self.z[1] = self.b[2] * x - self.a[1] * y;
        y
    }
}

/// Q values of the two sections of a 4th-order Butterworth filter.
pub const BUTTERWORTH4_Q: [f32; 2] = [0.541_196_1, 1.306_563];
