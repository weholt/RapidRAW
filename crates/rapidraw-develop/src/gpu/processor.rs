//! The ordered GPU adjustment pipeline, moved verbatim from RapidRAW
//! `src-tauri/src/gpu_processing.rs::GpuProcessor` (with only error-typing
//! and context-ownership changes). The processing order, tiling geometry,
//! blur radii and shader ABI are untouched so extraction does not change
//! image appearance.

use half::f16;
use image::DynamicImage;
use wgpu::util::{DeviceExt, TextureDataOrder};

use super::context::OffscreenGpuContext;
use super::error::GpuError;
use super::params::{AllAdjustments, MAX_MASKS};
use super::renderer::Roi;

#[repr(C)]
#[derive(Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct BlurParams {
    radius: u32,
    tile_offset_x: u32,
    tile_offset_y: u32,
    input_width: u32,
    input_height: u32,
    _pad1: u32,
    _pad2: u32,
    _pad3: u32,
}

#[repr(C)]
#[derive(Debug, Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct FlareParams {
    amount: f32,
    is_raw: u32,
    exposure: f32,
    brightness: f32,
    contrast: f32,
    whites: f32,
    aspect_ratio: f32,
    _pad: f32,
}

pub const FLARE_MAP_SIZE: u32 = 512;
const TILE_SIZE: u32 = 2048;
const TILE_OVERLAP: u32 = 128;

/// Readback of a texture region as tightly packed RGBA8.
pub(crate) fn read_texture_data_roi(
    context: &OffscreenGpuContext,
    texture: &wgpu::Texture,
    origin: wgpu::Origin3d,
    size: wgpu::Extent3d,
) -> Result<Vec<u8>, GpuError> {
    let device = context.device();
    let queue = context.queue();
    let unpadded_bytes_per_row = 4 * size.width;
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT;
    let padded_bytes_per_row = (unpadded_bytes_per_row + align - 1) & !(align - 1);
    let output_buffer_size = (padded_bytes_per_row * size.height) as u64;

    let output_buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("Readback Buffer"),
        size: output_buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });

    let mut encoder =
        device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
    encoder.copy_texture_to_buffer(
        wgpu::TexelCopyTextureInfo {
            texture,
            mip_level: 0,
            origin,
            aspect: wgpu::TextureAspect::All,
        },
        wgpu::TexelCopyBufferInfo {
            buffer: &output_buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_bytes_per_row),
                rows_per_image: Some(size.height),
            },
        },
        size,
    );

    queue.submit(Some(encoder.finish()));
    let buffer_slice = output_buffer.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    buffer_slice.map_async(wgpu::MapMode::Read, move |result| {
        let _ = tx.send(result);
    });
    device
        .poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(std::time::Duration::from_secs(60)),
        })
        .map_err(|e| GpuError::MapFailed(format!("failed while polling mapped GPU buffer: {e}")))?;
    let map_result = rx
        .recv()
        .map_err(|e| GpuError::MapFailed(format!("failed receiving GPU map result: {e}")))?;
    map_result.map_err(|e| GpuError::MapFailed(e.to_string()))?;

    let padded_data = buffer_slice.get_mapped_range().to_vec();
    output_buffer.unmap();

    if let Some(err) = context.take_uncaptured_errors() {
        return Err(err);
    }

    if padded_bytes_per_row == unpadded_bytes_per_row {
        Ok(padded_data)
    } else {
        let mut unpadded_data = Vec::with_capacity((unpadded_bytes_per_row * size.height) as usize);
        for chunk in padded_data.chunks(padded_bytes_per_row as usize) {
            unpadded_data.extend_from_slice(&chunk[..unpadded_bytes_per_row as usize]);
        }
        Ok(unpadded_data)
    }
}

pub(crate) fn to_rgba_f16(img: &DynamicImage) -> Vec<f16> {
    let rgba_f32 = img.to_rgba32f();
    rgba_f32.into_raw().into_iter().map(f16::from_f32).collect()
}

/// Upload the base image as an Rgba16Float texture (the pipeline's working
/// precision, matching the pre-extraction host upload path).
pub(crate) fn upload_input_texture(
    context: &OffscreenGpuContext,
    base: &DynamicImage,
    width: u32,
    height: u32,
) -> Result<(wgpu::Texture, wgpu::TextureView), GpuError> {
    let img_rgba_f16 = to_rgba_f16(base);
    let texture = context.device().create_texture_with_data(
        context.queue(),
        &wgpu::TextureDescriptor {
            label: Some("Input Texture"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        },
        TextureDataOrder::MipMajor,
        bytemuck::cast_slice(&img_rgba_f16),
    );
    if let Some(err) = context.take_uncaptured_errors() {
        return Err(err);
    }
    let view = texture.create_view(&Default::default());
    Ok((texture, view))
}

pub struct GpuProcessor {
    pub context: OffscreenGpuContext,
    blur_bgl: wgpu::BindGroupLayout,
    h_blur_pipeline: wgpu::ComputePipeline,
    v_blur_pipeline: wgpu::ComputePipeline,
    blur_params_buffer: wgpu::Buffer,

    flare_bgl_0: wgpu::BindGroupLayout,
    flare_bgl_1: wgpu::BindGroupLayout,
    flare_threshold_pipeline: wgpu::ComputePipeline,
    flare_ghosts_pipeline: wgpu::ComputePipeline,
    flare_params_buffer: wgpu::Buffer,
    flare_threshold_view: wgpu::TextureView,
    flare_ghosts_view: wgpu::TextureView,
    flare_final_view: wgpu::TextureView,
    flare_sampler: wgpu::Sampler,

    main_bgl: wgpu::BindGroupLayout,
    main_pipeline: wgpu::ComputePipeline,
    adjustments_buffer: wgpu::Buffer,
    dummy_blur_view: wgpu::TextureView,
    dummy_lut_view: wgpu::TextureView,
    dummy_lut_sampler: wgpu::Sampler,
    ping_pong_view: wgpu::TextureView,
    sharpness_blur_view: wgpu::TextureView,
    tonal_blur_view: wgpu::TextureView,
    clarity_blur_view: wgpu::TextureView,
    structure_blur_view: wgpu::TextureView,

    pub tile_output_texture: wgpu::Texture,
    pub tile_output_texture_view: wgpu::TextureView,
    pub working_texture: wgpu::Texture,
    /// View over [`GpuProcessor::working_texture`]; kept for shader-binding
    /// parity with the pre-extraction processor. Not currently bound.
    #[allow(dead_code)]
    pub working_texture_view: wgpu::TextureView,
    pub output_texture: wgpu::Texture,
    pub output_texture_view: wgpu::TextureView,
}

impl GpuProcessor {
    pub fn new(
        context: &OffscreenGpuContext,
        max_width: u32,
        max_height: u32,
    ) -> Result<Self, GpuError> {
        let device = context.device();
        const MAX_MASK_BINDINGS: u32 = 1;

        let blur_shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Blur Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/blur.wgsl").into()),
        });

        let blur_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Blur BGL"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });

        let blur_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Blur Pipeline Layout"),
            bind_group_layouts: &[Some(&blur_bgl)],
            immediate_size: 0,
        });

        let h_blur_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Horizontal Blur Pipeline"),
            layout: Some(&blur_pipeline_layout),
            module: &blur_shader_module,
            entry_point: Some("horizontal_blur"),
            compilation_options: Default::default(),
            cache: None,
        });

        let v_blur_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Vertical Blur Pipeline"),
            layout: Some(&blur_pipeline_layout),
            module: &blur_shader_module,
            entry_point: Some("vertical_blur"),
            compilation_options: Default::default(),
            cache: None,
        });

        let blur_params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Blur Params Buffer"),
            size: std::mem::size_of::<BlurParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let flare_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Flare Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/flare.wgsl").into()),
        });

        let flare_bgl_0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Flare BGL 0"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });

        let flare_bgl_1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Flare BGL 1"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::COMPUTE,
                    ty: wgpu::BindingType::StorageTexture {
                        access: wgpu::StorageTextureAccess::WriteOnly,
                        format: wgpu::TextureFormat::Rgba16Float,
                        view_dimension: wgpu::TextureViewDimension::D2,
                    },
                    count: None,
                },
            ],
        });

        let flare_threshold_layout =
            device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("Flare Threshold Layout"),
                bind_group_layouts: &[Some(&flare_bgl_0)],
                immediate_size: 0,
            });

        let flare_ghosts_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Flare Ghosts Layout"),
            bind_group_layouts: &[Some(&flare_bgl_0), Some(&flare_bgl_1)],
            immediate_size: 0,
        });

        let flare_threshold_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("Flare Threshold Pipeline"),
                layout: Some(&flare_threshold_layout),
                module: &flare_shader,
                entry_point: Some("threshold_main"),
                compilation_options: Default::default(),
                cache: None,
            });

        let flare_ghosts_pipeline =
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some("Flare Ghosts Pipeline"),
                layout: Some(&flare_ghosts_layout),
                module: &flare_shader,
                entry_point: Some("ghosts_main"),
                compilation_options: Default::default(),
                cache: None,
            });

        let flare_params_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Flare Params Buffer"),
            size: std::mem::size_of::<FlareParams>() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let flare_tex_desc = wgpu::TextureDescriptor {
            label: Some("Flare Tex"),
            size: wgpu::Extent3d {
                width: FLARE_MAP_SIZE,
                height: FLARE_MAP_SIZE,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        };

        let flare_threshold_texture = device.create_texture(&flare_tex_desc);
        let flare_threshold_view = flare_threshold_texture.create_view(&Default::default());
        let flare_ghosts_texture = device.create_texture(&flare_tex_desc);
        let flare_ghosts_view = flare_ghosts_texture.create_view(&Default::default());
        let flare_final_texture = device.create_texture(&flare_tex_desc);
        let flare_final_view = flare_final_texture.create_view(&Default::default());

        let flare_sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("Flare Sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            ..Default::default()
        });

        let shader_module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("Image Processing Shader"),
            source: wgpu::ShaderSource::Wgsl(include_str!("shaders/shader.wgsl").into()),
        });

        let mut bind_group_layout_entries = vec![
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: false },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::StorageTexture {
                    access: wgpu::StorageTextureAccess::WriteOnly,
                    format: wgpu::TextureFormat::Rgba8Unorm,
                    view_dimension: wgpu::TextureViewDimension::D2,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            },
        ];

        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 3,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2Array,
                multisampled: false,
            },
            count: None,
        });

        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 3 + MAX_MASK_BINDINGS,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D3,
                multisampled: false,
            },
            count: None,
        });
        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 4 + MAX_MASK_BINDINGS,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::NonFiltering),
            count: None,
        });

        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 5 + MAX_MASK_BINDINGS,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 6 + MAX_MASK_BINDINGS,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 7 + MAX_MASK_BINDINGS,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 8 + MAX_MASK_BINDINGS,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: false },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });

        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 9 + MAX_MASK_BINDINGS,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        });
        bind_group_layout_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 10 + MAX_MASK_BINDINGS,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        });

        let main_bgl = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("Main BGL"),
            entries: &bind_group_layout_entries,
        });

        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("Pipeline Layout"),
            bind_group_layouts: &[Some(&main_bgl)],
            immediate_size: 0,
        });

        let main_pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("Compute Pipeline"),
            layout: Some(&pipeline_layout),
            module: &shader_module,
            entry_point: Some("main"),
            compilation_options: Default::default(),
            cache: None,
        });

        let adjustments_buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("Adjustments Buffer"),
            size: std::mem::size_of::<AllAdjustments>() as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        let dummy_texture_desc = wgpu::TextureDescriptor {
            label: Some("Dummy Texture"),
            size: wgpu::Extent3d {
                width: 1,
                height: 1,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        };
        let dummy_blur_texture = device.create_texture(&dummy_texture_desc);
        let dummy_blur_view = dummy_blur_texture.create_view(&Default::default());

        let dummy_lut_texture = device.create_texture(&wgpu::TextureDescriptor {
            dimension: wgpu::TextureDimension::D3,
            ..dummy_texture_desc
        });
        let dummy_lut_view = dummy_lut_texture.create_view(&Default::default());
        let dummy_lut_sampler = device.create_sampler(&wgpu::SamplerDescriptor::default());

        let clamped_tile_width = max_width.min(TILE_SIZE + TILE_OVERLAP * 2);
        let clamped_tile_height = max_height.min(TILE_SIZE + TILE_OVERLAP * 2);

        let clamped_tile_size = wgpu::Extent3d {
            width: clamped_tile_width,
            height: clamped_tile_height,
            depth_or_array_layers: 1,
        };

        let full_image_size = wgpu::Extent3d {
            width: max_width,
            height: max_height,
            depth_or_array_layers: 1,
        };

        let reusable_texture_desc = wgpu::TextureDescriptor {
            label: None,
            size: clamped_tile_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
            view_formats: &[],
        };

        let ping_pong_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Ping Pong Texture"),
            ..reusable_texture_desc
        });
        let ping_pong_view = ping_pong_texture.create_view(&Default::default());

        let sharpness_blur_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Sharpness Blur Texture"),
            ..reusable_texture_desc
        });
        let sharpness_blur_view = sharpness_blur_texture.create_view(&Default::default());

        let tonal_blur_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Tonal Blur Texture"),
            ..reusable_texture_desc
        });
        let tonal_blur_view = tonal_blur_texture.create_view(&Default::default());

        let clarity_blur_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Clarity Blur Texture"),
            ..reusable_texture_desc
        });
        let clarity_blur_view = clarity_blur_texture.create_view(&Default::default());

        let structure_blur_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Structure Blur Texture"),
            ..reusable_texture_desc
        });
        let structure_blur_view = structure_blur_texture.create_view(&Default::default());

        let tile_output_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Tile Output Texture"),
            size: clamped_tile_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::STORAGE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let tile_output_texture_view = tile_output_texture.create_view(&Default::default());

        let working_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Working Output Texture"),
            size: full_image_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let working_texture_view = working_texture.create_view(&Default::default());

        let output_texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("Full Output Texture"),
            size: full_image_size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_DST
                | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let output_texture_view = output_texture.create_view(&Default::default());

        if let Some(err) = context.take_uncaptured_errors() {
            return Err(err);
        }

        Ok(Self {
            context: context.clone(),
            blur_bgl,
            h_blur_pipeline,
            v_blur_pipeline,
            blur_params_buffer,
            flare_bgl_0,
            flare_bgl_1,
            flare_threshold_pipeline,
            flare_ghosts_pipeline,
            flare_params_buffer,
            flare_threshold_view,
            flare_ghosts_view,
            flare_final_view,
            flare_sampler,
            main_bgl,
            main_pipeline,
            adjustments_buffer,
            dummy_blur_view,
            dummy_lut_view,
            dummy_lut_sampler,
            ping_pong_view,
            sharpness_blur_view,
            tonal_blur_view,
            clarity_blur_view,
            structure_blur_view,
            tile_output_texture,
            tile_output_texture_view,
            working_texture,
            working_texture_view,
            output_texture,
            output_texture_view,
        })
    }

    /// Run the ordered pipeline over the input texture. Returns
    /// `(pixels, out_width, out_height, out_x, out_y)`; `pixels` is empty
    /// when `skip_cpu_readback` is set.
    #[allow(clippy::type_complexity)]
    pub fn run(
        &self,
        input_texture_view: &wgpu::TextureView,
        width: u32,
        height: u32,
        request: super::renderer::RenderRequest<'_>,
        skip_cpu_readback: bool,
        output_to_display: bool,
    ) -> Result<(Vec<u8>, u32, u32, u32, u32), GpuError> {
        let context = &self.context;
        let device = context.device();
        let queue = context.queue();
        let scale = (width.min(height) as f32) / 1080.0;
        const MAX_MASK_BINDINGS: u32 = 1;

        if context.is_device_lost() {
            return Err(GpuError::DeviceLost(
                "device lost before dispatch".to_string(),
            ));
        }

        let bounds = request.roi.unwrap_or(Roi {
            x: 0,
            y: 0,
            width,
            height,
        });
        let out_width = bounds.width;
        let out_height = bounds.height;
        let mask_layer_count = request.mask_bitmaps.len().clamp(2, MAX_MASKS) as u32;
        let full_texture_size = wgpu::Extent3d {
            width,
            height,
            depth_or_array_layers: mask_layer_count,
        };
        let buffer_size = (width as usize) * (height as usize) * (mask_layer_count as usize);
        let mut mask_texture_data = Vec::with_capacity(buffer_size);
        if request.mask_bitmaps.is_empty() {
            mask_texture_data.resize(buffer_size, 0);
        } else {
            for mask_bitmap in request.mask_bitmaps.iter().take(MAX_MASKS) {
                mask_texture_data.extend_from_slice(mask_bitmap.as_raw());
            }
            if mask_texture_data.len() < buffer_size {
                mask_texture_data.resize(buffer_size, 0);
            }
        }
        let mask_texture = device.create_texture_with_data(
            queue,
            &wgpu::TextureDescriptor {
                label: Some("Full Mask Texture Array"),
                size: full_texture_size,
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::R8Unorm,
                usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                view_formats: &[],
            },
            TextureDataOrder::MipMajor,
            &mask_texture_data,
        );
        let mask_texture_view = mask_texture.create_view(&wgpu::TextureViewDescriptor {
            dimension: Some(wgpu::TextureViewDimension::D2Array),
            ..Default::default()
        });

        let (lut_texture_view, lut_sampler) = if let Some(lut) = &request.lut {
            let lut_data = &lut.data;
            let size = lut.size;
            let mut rgba_lut_data_f16 = Vec::with_capacity(lut_data.len() / 3 * 4);
            for chunk in lut_data.as_chunks::<3>().0 {
                rgba_lut_data_f16.push(f16::from_f32(chunk[0]));
                rgba_lut_data_f16.push(f16::from_f32(chunk[1]));
                rgba_lut_data_f16.push(f16::from_f32(chunk[2]));
                rgba_lut_data_f16.push(f16::ONE);
            }
            let lut_texture = device.create_texture_with_data(
                queue,
                &wgpu::TextureDescriptor {
                    label: Some("LUT 3D Texture"),
                    size: wgpu::Extent3d {
                        width: size,
                        height: size,
                        depth_or_array_layers: size,
                    },
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D3,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
                    view_formats: &[],
                },
                TextureDataOrder::MipMajor,
                bytemuck::cast_slice(&rgba_lut_data_f16),
            );
            let view = lut_texture.create_view(&Default::default());
            let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
                address_mode_u: wgpu::AddressMode::ClampToEdge,
                address_mode_v: wgpu::AddressMode::ClampToEdge,
                address_mode_w: wgpu::AddressMode::ClampToEdge,
                mag_filter: wgpu::FilterMode::Nearest,
                min_filter: wgpu::FilterMode::Nearest,
                ..Default::default()
            });
            (view, sampler)
        } else {
            (self.dummy_lut_view.clone(), self.dummy_lut_sampler.clone())
        };

        let adjustments = request.adjustments;
        if adjustments.global.flare_amount > 0.0 {
            let mut encoder = device.create_command_encoder(&Default::default());

            let aspect_ratio = if height > 0 {
                width as f32 / height as f32
            } else {
                1.0
            };
            let f_params = FlareParams {
                amount: adjustments.global.flare_amount,
                is_raw: adjustments.global.is_raw_image,
                exposure: adjustments.global.exposure,
                brightness: adjustments.global.brightness,
                contrast: adjustments.global.contrast,
                whites: adjustments.global.whites,
                aspect_ratio,
                _pad: 0.0,
            };
            queue.write_buffer(&self.flare_params_buffer, 0, bytemuck::bytes_of(&f_params));

            let bg0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Flare BG0"),
                layout: &self.flare_bgl_0,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(input_texture_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&self.flare_threshold_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.flare_params_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.flare_sampler),
                    },
                ],
            });

            {
                let mut cpass = encoder.begin_compute_pass(&Default::default());
                cpass.set_pipeline(&self.flare_threshold_pipeline);
                cpass.set_bind_group(0, &bg0, &[]);
                cpass.dispatch_workgroups(FLARE_MAP_SIZE / 16, FLARE_MAP_SIZE / 16, 1);
            }

            let bg0_ghosts = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Flare BG0 Ghosts"),
                layout: &self.flare_bgl_0,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(input_texture_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&self.flare_final_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.flare_params_buffer.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(&self.flare_sampler),
                    },
                ],
            });

            let bg1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("Flare BG1"),
                layout: &self.flare_bgl_1,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&self.flare_threshold_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&self.flare_ghosts_view),
                    },
                ],
            });

            {
                let mut cpass = encoder.begin_compute_pass(&Default::default());
                cpass.set_pipeline(&self.flare_ghosts_pipeline);
                cpass.set_bind_group(0, &bg0_ghosts, &[]);
                cpass.set_bind_group(1, &bg1, &[]);
                cpass.dispatch_workgroups(FLARE_MAP_SIZE / 16, FLARE_MAP_SIZE / 16, 1);
            }

            queue.submit(Some(encoder.finish()));
        }

        let mut final_pixels = vec![
            0u8;
            if skip_cpu_readback {
                0
            } else {
                (out_width * out_height * 4) as usize
            }
        ];

        let start_tile_x = bounds.x / TILE_SIZE;
        let start_tile_y = bounds.y / TILE_SIZE;
        let end_tile_x = (bounds.x + bounds.width).div_ceil(TILE_SIZE);
        let end_tile_y = (bounds.y + bounds.height).div_ceil(TILE_SIZE);

        for tile_y in start_tile_y..end_tile_y {
            for tile_x in start_tile_x..end_tile_x {
                let x_start_unclamped = tile_x * TILE_SIZE;
                let y_start_unclamped = tile_y * TILE_SIZE;

                let x_start = x_start_unclamped.max(bounds.x);
                let y_start = y_start_unclamped.max(bounds.y);
                let x_end = (x_start_unclamped + TILE_SIZE)
                    .min(bounds.x + bounds.width)
                    .min(width);
                let y_end = (y_start_unclamped + TILE_SIZE)
                    .min(bounds.y + bounds.height)
                    .min(height);

                let tile_width = x_end - x_start;
                let tile_height = y_end - y_start;

                let input_x_start = (x_start as i32 - TILE_OVERLAP as i32).max(0) as u32;
                let input_y_start = (y_start as i32 - TILE_OVERLAP as i32).max(0) as u32;
                let input_x_end = (x_end + TILE_OVERLAP).min(width);
                let input_y_end = (y_end + TILE_OVERLAP).min(height);
                let input_width = input_x_end - input_x_start;
                let input_height = input_y_end - input_y_start;

                let input_texture_size = wgpu::Extent3d {
                    width: input_width,
                    height: input_height,
                    depth_or_array_layers: 1,
                };

                let run_blur = |base_radius: f32, output_view: &wgpu::TextureView| -> bool {
                    let radius = (base_radius * scale).ceil().max(1.0) as u32;
                    if radius == 0 {
                        return false;
                    }

                    let params = BlurParams {
                        radius,
                        tile_offset_x: input_x_start,
                        tile_offset_y: input_y_start,
                        input_width,
                        input_height,
                        _pad1: 0,
                        _pad2: 0,
                        _pad3: 0,
                    };
                    queue.write_buffer(&self.blur_params_buffer, 0, bytemuck::bytes_of(&params));

                    let mut blur_encoder = device.create_command_encoder(&Default::default());

                    let h_blur_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("H-Blur BG"),
                        layout: &self.blur_bgl,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(input_texture_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(&self.ping_pong_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: self.blur_params_buffer.as_entire_binding(),
                            },
                        ],
                    });

                    {
                        let mut cpass = blur_encoder.begin_compute_pass(&Default::default());
                        cpass.set_pipeline(&self.h_blur_pipeline);
                        cpass.set_bind_group(0, &h_blur_bg, &[]);
                        cpass.dispatch_workgroups(input_width.div_ceil(256), input_height, 1);
                    }

                    let v_blur_bg = device.create_bind_group(&wgpu::BindGroupDescriptor {
                        label: Some("V-Blur BG"),
                        layout: &self.blur_bgl,
                        entries: &[
                            wgpu::BindGroupEntry {
                                binding: 0,
                                resource: wgpu::BindingResource::TextureView(&self.ping_pong_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 1,
                                resource: wgpu::BindingResource::TextureView(output_view),
                            },
                            wgpu::BindGroupEntry {
                                binding: 2,
                                resource: self.blur_params_buffer.as_entire_binding(),
                            },
                        ],
                    });

                    {
                        let mut cpass = blur_encoder.begin_compute_pass(&Default::default());
                        cpass.set_pipeline(&self.v_blur_pipeline);
                        cpass.set_bind_group(0, &v_blur_bg, &[]);
                        cpass.dispatch_workgroups(input_width, input_height.div_ceil(256), 1);
                    }

                    queue.submit(Some(blur_encoder.finish()));
                    true
                };

                let did_create_sharpness_blur = run_blur(1.0, &self.sharpness_blur_view);
                let did_create_tonal_blur = run_blur(3.5, &self.tonal_blur_view);
                let did_create_clarity_blur = run_blur(8.0, &self.clarity_blur_view);
                let did_create_structure_blur = run_blur(40.0, &self.structure_blur_view);

                let mut main_encoder = device.create_command_encoder(&Default::default());

                let mut tile_adjustments = adjustments;
                tile_adjustments.tile_offset_x = input_x_start;
                tile_adjustments.tile_offset_y = input_y_start;
                queue.write_buffer(
                    &self.adjustments_buffer,
                    0,
                    bytemuck::bytes_of(&tile_adjustments),
                );

                let mut bind_group_entries = vec![
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(input_texture_view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(
                            &self.tile_output_texture_view,
                        ),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: self.adjustments_buffer.as_entire_binding(),
                    },
                ];
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::TextureView(&mask_texture_view),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 3 + MAX_MASK_BINDINGS,
                    resource: wgpu::BindingResource::TextureView(&lut_texture_view),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 4 + MAX_MASK_BINDINGS,
                    resource: wgpu::BindingResource::Sampler(&lut_sampler),
                });

                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 5 + MAX_MASK_BINDINGS,
                    resource: wgpu::BindingResource::TextureView(if did_create_sharpness_blur {
                        &self.sharpness_blur_view
                    } else {
                        &self.dummy_blur_view
                    }),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 6 + MAX_MASK_BINDINGS,
                    resource: wgpu::BindingResource::TextureView(if did_create_tonal_blur {
                        &self.tonal_blur_view
                    } else {
                        &self.dummy_blur_view
                    }),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 7 + MAX_MASK_BINDINGS,
                    resource: wgpu::BindingResource::TextureView(if did_create_clarity_blur {
                        &self.clarity_blur_view
                    } else {
                        &self.dummy_blur_view
                    }),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 8 + MAX_MASK_BINDINGS,
                    resource: wgpu::BindingResource::TextureView(if did_create_structure_blur {
                        &self.structure_blur_view
                    } else {
                        &self.dummy_blur_view
                    }),
                });

                let use_flare = adjustments.global.flare_amount > 0.0;
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 9 + MAX_MASK_BINDINGS,
                    resource: wgpu::BindingResource::TextureView(if use_flare {
                        &self.flare_ghosts_view
                    } else {
                        &self.dummy_blur_view
                    }),
                });
                bind_group_entries.push(wgpu::BindGroupEntry {
                    binding: 10 + MAX_MASK_BINDINGS,
                    resource: wgpu::BindingResource::Sampler(&self.flare_sampler),
                });

                let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("Tile Bind Group"),
                    layout: &self.main_bgl,
                    entries: &bind_group_entries,
                });

                {
                    let mut compute_pass = main_encoder.begin_compute_pass(&Default::default());
                    compute_pass.set_pipeline(&self.main_pipeline);
                    compute_pass.set_bind_group(0, &bind_group, &[]);
                    compute_pass.dispatch_workgroups(
                        input_width.div_ceil(8),
                        input_height.div_ceil(8),
                        1,
                    );
                }

                let crop_x_start = x_start - input_x_start;
                let crop_y_start = y_start - input_y_start;

                if output_to_display {
                    main_encoder.copy_texture_to_texture(
                        wgpu::TexelCopyTextureInfo {
                            texture: &self.tile_output_texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: crop_x_start,
                                y: crop_y_start,
                                z: 0,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::TexelCopyTextureInfo {
                            texture: &self.working_texture,
                            mip_level: 0,
                            origin: wgpu::Origin3d {
                                x: x_start,
                                y: y_start,
                                z: 0,
                            },
                            aspect: wgpu::TextureAspect::All,
                        },
                        wgpu::Extent3d {
                            width: tile_width,
                            height: tile_height,
                            depth_or_array_layers: 1,
                        },
                    );
                }

                queue.submit(Some(main_encoder.finish()));

                if !skip_cpu_readback {
                    let processed_tile_data = read_texture_data_roi(
                        context,
                        &self.tile_output_texture,
                        wgpu::Origin3d::ZERO,
                        input_texture_size,
                    )?;

                    for row in 0..tile_height {
                        let final_y = y_start + row - bounds.y;
                        let final_x = x_start - bounds.x;
                        let final_row_offset = (final_y * out_width + final_x) as usize * 4;
                        let source_y = crop_y_start + row;
                        let source_row_offset =
                            (source_y * input_width + crop_x_start) as usize * 4;
                        let copy_bytes = (tile_width * 4) as usize;

                        final_pixels[final_row_offset..final_row_offset + copy_bytes]
                            .copy_from_slice(
                                &processed_tile_data
                                    [source_row_offset..source_row_offset + copy_bytes],
                            );
                    }
                }
            }
        }

        if let Some(err) = context.take_uncaptured_errors() {
            return Err(err);
        }

        Ok((final_pixels, out_width, out_height, bounds.x, bounds.y))
    }
}
