//! Offscreen renderer: owns the device context and the bounded GPU resource
//! caches, resolves recipe resources explicitly, and runs the ordered WGSL
//! pipeline. This is the extraction of RapidRAW's
//! `gpu_processing.rs::GpuProcessor` plus the cache half of
//! `process_and_get_dynamic_image_inner`; presentation is NOT part of this
//! module.

use std::sync::Mutex;

use image::{DynamicImage, GenericImageView};

use super::context::OffscreenGpuContext;
use super::error::GpuError;
use super::processor::{GpuProcessor, upload_input_texture};

/// Region of interest into the full texture, in source pixels. `None` means
/// the whole image.
#[derive(Clone, Copy, Debug)]
pub struct Roi {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

/// Neutral LUT payload resolved by the host from its resource store. The
/// crate receives the decoded cube values (`size^3` RGB f32 triplets); it
/// never touches the filesystem, so a missing LUT is a typed
/// [`GpuError::ResourceMissing`] rather than a silent dummy texture.
#[derive(Clone, Debug)]
pub struct LutData {
    pub size: u32,
    pub data: Vec<f32>,
}

/// One offscreen render request, resolved: everything the ordered pipeline
/// needs. Mask bitmaps are plain host-decoded data (no host cache types).
pub struct RenderRequest<'a> {
    pub adjustments: super::params::AllAdjustments,
    pub mask_bitmaps: &'a [image::ImageBuffer<image::Luma<u8>, Vec<u8>>],
    pub lut: Option<std::sync::Arc<LutData>>,
    pub roi: Option<Roi>,
}

/// What the caller wants done with the rendered tiles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputTarget {
    /// Read the rendered region back to CPU pixels (export path).
    CpuPixels,
    /// Keep the result on the GPU output texture (host display path); no
    /// CPU readback is performed.
    DisplayTexture,
}

/// Rendered result. `pixels` is RGBA8 in [`OutputColorSpace::Rgba8Srgb`]
/// encoding and is empty for [`OutputTarget::DisplayTexture`].
#[derive(Debug, Clone)]
pub struct RenderedPixels {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub x: u32,
    pub y: u32,
}

impl RenderedPixels {
    /// Color-transform contract of `pixels` (see
    /// [`super::analytics::OutputColorSpace`]).
    pub fn color_space(&self) -> super::analytics::OutputColorSpace {
        super::analytics::OutputColorSpace::Rgba8Srgb
    }
}

/// Pipeline allocation entry. Bound: exactly one allocation per renderer,
/// grown (never shrunk, never accumulated) when a larger image arrives —
/// the same policy as the pre-extraction host.
struct ProcessorEntry {
    processor: GpuProcessor,
    width: u32,
    height: u32,
}

/// Uploaded base-image entry. Bound: exactly one entry per renderer, keyed
/// by (width, height, transform hash); a different key replaces it.
struct InputEntry {
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    width: u32,
    height: u32,
    transform_hash: u64,
}

/// Offscreen renderer with bounded caches. The caches are bounded by
/// construction: one pipeline allocation and one uploaded base texture per
/// renderer instance, replaced (never accumulated) on key change — the same
/// single-slot policy as the pre-extraction host AppState caches, now owned
/// by the engine instead of the application.
pub struct OffscreenRenderer {
    context: OffscreenGpuContext,
    processor: Mutex<Option<ProcessorEntry>>,
    input: Mutex<Option<InputEntry>>,
}

impl OffscreenRenderer {
    pub fn new(context: OffscreenGpuContext) -> Self {
        Self {
            context,
            processor: Mutex::new(None),
            input: Mutex::new(None),
        }
    }

    pub fn context(&self) -> &OffscreenGpuContext {
        &self.context
    }

    /// Capability report for this renderer's device.
    pub fn capabilities(&self) -> super::context::GpuCapabilities {
        self.context.capabilities()
    }

    /// Drop the cached uploaded base texture (e.g. when the host switches
    /// source images). The pipeline allocation is kept.
    pub fn clear_image_cache(&self) {
        if let Ok(mut slot) = self.input.lock() {
            *slot = None;
        }
    }

    /// Render `base` through the ordered adjustment pipeline.
    ///
    /// Failure modes are typed: oversized dimensions yield
    /// [`GpuError::TextureTooLarge`] (never the unprocessed base image), a
    /// recipe referencing a LUT without a resolved [`LutData`] yields
    /// [`GpuError::ResourceMissing`], and device loss and OOM surface as
    /// their typed variants.
    pub fn render(
        &self,
        base: &DynamicImage,
        transform_hash: u64,
        request: RenderRequest<'_>,
        target: OutputTarget,
    ) -> Result<RenderedPixels, GpuError> {
        let (width, height) = base.dimensions();
        if width == 0 || height == 0 {
            return Err(GpuError::InvalidRequest(format!(
                "cannot render empty base image {width}x{height}"
            )));
        }
        // Honest capability check: an oversized image is an explicit
        // failure, never a silent fallback to the unprocessed base image.
        self.context.check_texture_support(width, height)?;
        if self.context.is_device_lost() {
            return Err(GpuError::DeviceLost(
                "device lost before render".to_string(),
            ));
        }
        // Explicit resource resolution: the recipe flagging a LUT requires
        // the resolved LUT payload to be present in the request.
        if request.adjustments.global.has_lut == 1 && request.lut.is_none() {
            return Err(GpuError::ResourceMissing {
                kind: "lut",
                detail: "recipe enables a LUT (has_lut) but no LUT data was resolved".to_string(),
            });
        }

        // Bounded pipeline allocation: single slot, grown on demand.
        let alloc_width = (width + 255) & !255;
        let alloc_height = (height + 255) & !255;
        {
            let mut slot = self
                .processor
                .lock()
                .map_err(|_| GpuError::Internal("renderer processor lock poisoned".to_string()))?;
            let needs_new = slot
                .as_ref()
                .is_none_or(|e| e.width < width || e.height < height);
            if needs_new {
                let old = slot.take();
                drop(old);
                let _ = self.context.device().poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(std::time::Duration::from_millis(500)),
                });
                *slot = Some(ProcessorEntry {
                    processor: GpuProcessor::new(&self.context, alloc_width, alloc_height)?,
                    width: alloc_width,
                    height: alloc_height,
                });
            }
        }

        // Bounded input cache: single slot keyed by size + transform hash.
        {
            let mut slot = self
                .input
                .lock()
                .map_err(|_| GpuError::Internal("renderer input lock poisoned".to_string()))?;
            let needs_new = slot.as_ref().is_none_or(|e| {
                e.transform_hash != transform_hash || e.width != width || e.height != height
            });
            if needs_new {
                let old = slot.take();
                drop(old);
                let _ = self.context.device().poll(wgpu::PollType::Wait {
                    submission_index: None,
                    timeout: Some(std::time::Duration::from_millis(500)),
                });
                let (texture, view) = upload_input_texture(&self.context, base, width, height)?;
                *slot = Some(InputEntry {
                    _texture: texture,
                    view,
                    width,
                    height,
                    transform_hash,
                });
            }
        }

        let processor_slot = self
            .processor
            .lock()
            .map_err(|_| GpuError::Internal("renderer processor lock poisoned".to_string()))?;
        let input_slot = self
            .input
            .lock()
            .map_err(|_| GpuError::Internal("renderer input lock poisoned".to_string()))?;
        let processor = &processor_slot
            .as_ref()
            .expect("processor ensured")
            .processor;
        let input = input_slot.as_ref().expect("input ensured");

        let skip_readback = target == OutputTarget::DisplayTexture;
        let (pixels, out_w, out_h, out_x, out_y) = processor.run(
            &input.view,
            input.width,
            input.height,
            request,
            skip_readback,
            skip_readback,
        )?;

        Ok(RenderedPixels {
            pixels,
            width: out_w,
            height: out_h,
            x: out_x,
            y: out_y,
        })
    }

    /// Texture view of the rendered output for the host adapter's display
    /// bind group (presentation stays host-side). Prefer
    /// [`OffscreenRenderer::with_display_output`], which holds the pipeline
    /// lock like the pre-extraction host did.
    pub fn output_texture_view(&self) -> Option<wgpu::TextureView> {
        let slot = self.processor.lock().ok()?;
        slot.as_ref()
            .map(|e| e.processor.output_texture_view.clone())
    }

    /// Working (tile-composited) texture handle for host-side needs. The
    /// handle is share-cloned; prefer
    /// [`OffscreenRenderer::copy_working_to_readback_buffer`] for async
    /// analytics.
    pub fn working_texture(&self) -> Option<wgpu::Texture> {
        let slot = self.processor.lock().ok()?;
        slot.as_ref().map(|e| e.processor.working_texture.clone())
    }

    /// Pipeline allocation size (the aligned working dimensions), for the
    /// host display transform.
    pub fn allocation_size(&self) -> Option<(u32, u32)> {
        let slot = self.processor.lock().ok()?;
        slot.as_ref().map(|e| (e.width, e.height))
    }

    /// Copy the composed working region into the display output texture.
    /// The host display path calls this after a successful
    /// [`OutputTarget::DisplayTexture`] render, mirroring the
    /// pre-extraction `working -> output` copy.
    pub fn publish_output_for_display(&self, output: &RenderedPixels) {
        if output.width == 0 || output.height == 0 {
            return;
        }
        let Ok(slot) = self.processor.lock() else {
            return;
        };
        let Some(entry) = slot.as_ref() else {
            return;
        };
        let device = self.context.device();
        let queue = self.context.queue();
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Final Passes Encoder"),
        });
        encoder.copy_texture_to_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &entry.processor.working_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: output.x,
                    y: output.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyTextureInfo {
                texture: &entry.processor.output_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: output.x,
                    y: output.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::Extent3d {
                width: output.width,
                height: output.height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));
    }

    /// Run `f` with the display-facing output texture view and the pipeline
    /// allocation size while holding the pipeline lock — the same lock
    /// discipline the pre-extraction host used between rendering and
    /// presenting. Presentation itself stays in the host adapter.
    pub fn with_display_output<R>(
        &self,
        f: impl FnOnce(&wgpu::TextureView, (u32, u32)) -> R,
    ) -> Option<R> {
        let slot = self.processor.lock().ok()?;
        let entry = slot.as_ref()?;
        Some(f(
            &entry.processor.output_texture_view,
            (entry.width, entry.height),
        ))
    }

    /// Enqueue a copy of the rendered working region into a mappable
    /// readback buffer for asynchronous analytics. Returns the buffer for
    /// the caller's reader thread; the texture reference never leaves this
    /// crate.
    pub fn copy_working_to_readback_buffer(&self, output: &RenderedPixels) -> Option<wgpu::Buffer> {
        if output.width == 0 || output.height == 0 {
            return None;
        }
        let slot = self.processor.lock().ok()?;
        let entry = slot.as_ref()?;
        let device = self.context.device();
        let queue = self.context.queue();

        let unpadded_bytes_per_row = 4 * output.width;
        let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
        let padded_bytes_per_row = (unpadded_bytes_per_row + align - 1) & !(align - 1);
        let output_buffer_size = (padded_bytes_per_row * output.height) as u64;

        let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Async Analytics Readback Buffer"),
            size: output_buffer_size,
            usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
            mapped_at_creation: false,
        });

        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        encoder.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo {
                texture: &entry.processor.working_texture,
                mip_level: 0,
                origin: wgpu::Origin3d {
                    x: output.x,
                    y: output.y,
                    z: 0,
                },
                aspect: wgpu::TextureAspect::All,
            },
            wgpu::TexelCopyBufferInfo {
                buffer: &output_buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(padded_bytes_per_row),
                    rows_per_image: Some(output.height),
                },
            },
            wgpu::Extent3d {
                width: output.width,
                height: output.height,
                depth_or_array_layers: 1,
            },
        );
        queue.submit(Some(encoder.finish()));
        Some(output_buffer)
    }
}
