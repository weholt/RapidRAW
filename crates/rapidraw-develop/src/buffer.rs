//! Ownership of linear, high-precision image buffers.
//!
//! [`LinearImage`] is the decode boundary's buffer type: interleaved RGB,
//! `f32` samples in linear light, at the full decoded dimensions. It is the
//! single owner of its pixel data; the host never reaches into decoder
//! internals. The `rgba32f` projection (opaque alpha) exists for parity with
//! the host's previous `DynamicImage::ImageRgba32F` representation.

#[derive(Debug, Clone, PartialEq)]
pub struct LinearImage {
    width: u32,
    height: u32,
    /// Interleaved RGB, `data[(y * width + x) * 3 + c]`, linear light.
    data: Vec<f32>,
}

impl LinearImage {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            data: vec![0.0; width as usize * height as usize * 3],
        }
    }

    pub fn from_fn(width: u32, height: u32, mut f: impl FnMut(u32, u32) -> [f32; 3]) -> Self {
        let mut image = Self::new(width, height);
        for y in 0..height {
            for x in 0..width {
                let [r, g, b] = f(x, y);
                let idx = (y as usize * width as usize + x as usize) * 3;
                image.data[idx] = r;
                image.data[idx + 1] = g;
                image.data[idx + 2] = b;
            }
        }
        image
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn dimensions(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Interleaved linear RGB samples, row-major.
    pub fn rgb(&self) -> &[f32] {
        &self.data
    }

    pub(crate) fn rgb_mut(&mut self) -> &mut [f32] {
        &mut self.data
    }

    pub fn pixel(&self, x: u32, y: u32) -> [f32; 3] {
        let idx = (y as usize * self.width as usize + x as usize) * 3;
        [self.data[idx], self.data[idx + 1], self.data[idx + 2]]
    }

    pub fn set_pixel(&mut self, x: u32, y: u32, value: [f32; 3]) {
        let idx = (y as usize * self.width as usize + x as usize) * 3;
        self.data[idx] = value[0];
        self.data[idx + 1] = value[1];
        self.data[idx + 2] = value[2];
    }

    /// Interleaved RGBA samples with opaque alpha, matching the host's
    /// previous `ImageRgba32F` layout pixel for pixel.
    pub fn rgba32f(&self) -> Vec<f32> {
        let pixels = self.width as usize * self.height as usize;
        let mut out = Vec::with_capacity(pixels * 4);
        for i in 0..pixels {
            out.push(self.data[i * 3]);
            out.push(self.data[i * 3 + 1]);
            out.push(self.data[i * 3 + 2]);
            out.push(1.0);
        }
        out
    }
}
