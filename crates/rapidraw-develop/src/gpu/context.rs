//! Offscreen wgpu context. The context owns a logical device, queue, and the
//! device limits used for capability reporting. It deliberately has **no**
//! surface, no native window handle, and no application state: presentation
//! (the host adapter's `WgpuDisplay`) lives entirely outside this crate.

use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use wgpu::{Backends, Device, Instance, InstanceDescriptor, Limits, Queue};

use super::error::GpuError;

/// Atomic flag states for device-loss reporting.
const DEVICE_LOST: u8 = 1;

#[derive(Debug, Clone)]
pub struct GpuCapabilities {
    /// Maximum width/height of a 2D texture on this device. Images larger
    /// than this are rejected with [`GpuError::TextureTooLarge`], never
    /// silently processed as the unprocessed base image.
    pub max_texture_dimension_2d: u32,
    pub max_buffer_size: u64,
    pub adapter_name: String,
    pub backend: String,
}

/// Shared, typed error sink for uncaptured wgpu errors and device loss.
/// Handlers are installed at context creation so that out-of-memory,
/// validation, internal and device-loss events become typed
/// [`GpuError`]s instead of panics or silent success.
#[derive(Default)]
struct ErrorState {
    uncaptured: Mutex<Vec<GpuError>>,
    device_lost: AtomicU8,
    device_lost_message: Mutex<Option<String>>,
}

/// Offscreen GPU context: device + queue + limits + capability reporting.
///
/// Created either standalone with [`OffscreenGpuContext::new`] (no display
/// involvement at all) or, for hosts that must share the device with their
/// own presentation surface, from already-initialized device parts with
/// [`OffscreenGpuContext::from_parts`]. Neither constructor stores or
/// requires a window handle.
pub struct OffscreenGpuContext {
    device: Arc<Device>,
    queue: Arc<Queue>,
    limits: Limits,
    adapter_name: String,
    backend: String,
    errors: Arc<ErrorState>,
}

impl Clone for OffscreenGpuContext {
    fn clone(&self) -> Self {
        Self {
            device: Arc::clone(&self.device),
            queue: Arc::clone(&self.queue),
            limits: self.limits.clone(),
            adapter_name: self.adapter_name.clone(),
            backend: self.backend.clone(),
            errors: Arc::clone(&self.errors),
        }
    }
}

impl OffscreenGpuContext {
    /// Create a fully offscreen context: instance without any display
    /// handle, adapter and device selected from the environment-selected
    /// backends. This path never touches a window and is the entry point
    /// required for a core-engine build with no application window (spec
    /// A11).
    pub fn new() -> Result<Self, GpuError> {
        let mut instance_desc = InstanceDescriptor::new_without_display_handle_from_env();
        #[cfg(target_os = "windows")]
        if std::env::var("WGPU_BACKEND").is_err() {
            instance_desc.backends = Backends::PRIMARY;
        }
        Self::new_with_instance_desc(instance_desc)
    }

    /// Like [`OffscreenGpuContext::new`] but with explicit backends. Used by
    /// tests and by callers with deterministic adapter-selection needs; an
    /// empty backend set deterministically yields [`GpuError::NoAdapter`].
    pub fn new_with_backends(backends: Backends) -> Result<Self, GpuError> {
        Self::new_with_instance_desc(InstanceDescriptor {
            backends,
            ..InstanceDescriptor::new_without_display_handle_from_env()
        })
    }

    fn new_with_instance_desc(instance_desc: InstanceDescriptor) -> Result<Self, GpuError> {
        let instance = Instance::new(instance_desc);
        Self::from_instance(&instance, None, "rapidraw-develop offscreen device")
    }

    /// Initialize from an existing instance, optionally constraining adapter
    /// selection to a surface that the *host adapter* owns. The crate never
    /// stores the surface, never configures it, and never renders to it; the
    /// reference is used only as an adapter-compatibility hint so a host can
    /// share one device between offscreen compute and its own presentation.
    /// Passing `None` is the pure offscreen path.
    pub fn from_instance(
        instance: &Instance,
        adapter_constraint: Option<&wgpu::Surface<'_>>,
        device_label: &str,
    ) -> Result<Self, GpuError> {
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: adapter_constraint,
            ..Default::default()
        }))
        .map_err(|e| GpuError::NoAdapter(e.to_string()))?;

        let adapter_name = adapter.get_info().name.clone();
        let backend = format!("{:?}", adapter.get_info().backend);
        let limits = adapter.limits();

        let mut required_features = wgpu::Features::empty();
        if adapter
            .features()
            .contains(wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES)
        {
            required_features |= wgpu::Features::TEXTURE_ADAPTER_SPECIFIC_FORMAT_FEATURES;
        }

        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some(device_label),
            required_features,
            required_limits: limits.clone(),
            experimental_features: wgpu::ExperimentalFeatures::default(),
            memory_hints: wgpu::MemoryHints::Performance,
            trace: wgpu::Trace::Off,
        }))
        .map_err(|e| GpuError::DeviceRequest(e.to_string()))?;

        Ok(Self::install_error_handlers(
            Arc::new(device),
            Arc::new(queue),
            limits,
            adapter_name,
            backend,
        ))
    }

    /// Adopt an already-initialized device/queue pair. This is the
    /// integration point for host adapters that own initialization (for
    /// example to keep a presentation surface on the same adapter). The
    /// context exposes the device for compute only; no presentation state is
    /// represented here.
    pub fn from_parts(device: Arc<Device>, queue: Arc<Queue>, limits: Limits) -> Self {
        let info = device.adapter_info();
        Self::install_error_handlers(
            device,
            queue,
            limits,
            info.name.clone(),
            format!("{:?}", info.backend),
        )
    }

    fn install_error_handlers(
        device: Arc<Device>,
        queue: Arc<Queue>,
        limits: Limits,
        adapter_name: String,
        backend: String,
    ) -> Self {
        let errors = Arc::new(ErrorState::default());
        let sink = Arc::clone(&errors);
        device.on_uncaptured_error(Arc::new(move |error| {
            let typed = match error {
                wgpu::Error::OutOfMemory { .. } => GpuError::OutOfMemory(error.to_string()),
                wgpu::Error::Validation { .. } => GpuError::InvalidRequest(error.to_string()),
                wgpu::Error::Internal { .. } => GpuError::Internal(error.to_string()),
            };
            if let Ok(mut slot) = sink.uncaptured.lock() {
                slot.push(typed);
            }
        }));
        let lost_sink = Arc::clone(&errors);
        device.set_device_lost_callback(move |_reason, message| {
            lost_sink.device_lost.store(DEVICE_LOST, Ordering::SeqCst);
            if let Ok(mut slot) = lost_sink.device_lost_message.lock() {
                *slot = Some(message);
            }
        });
        Self {
            device,
            queue,
            limits,
            adapter_name,
            backend,
            errors,
        }
    }

    pub fn device(&self) -> &Arc<Device> {
        &self.device
    }

    pub fn queue(&self) -> &Arc<Queue> {
        &self.queue
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    /// Explicit capability report for the session/open handshake: what this
    /// device supports, and therefore which inputs must be refused.
    pub fn capabilities(&self) -> GpuCapabilities {
        GpuCapabilities {
            max_texture_dimension_2d: self.limits.max_texture_dimension_2d,
            max_buffer_size: self.limits.max_buffer_size,
            adapter_name: self.adapter_name.clone(),
            backend: self.backend.clone(),
        }
    }

    /// Reject images exceeding the 2D texture limit with a typed
    /// [`GpuError::TextureTooLarge`]. This is the honest replacement for the
    /// historical fallback that returned the unprocessed base image.
    pub fn check_texture_support(&self, width: u32, height: u32) -> Result<(), GpuError> {
        GpuError::check_texture_dimensions(width, height, self.limits.max_texture_dimension_2d)
    }

    /// Whether the device-loss callback has fired.
    pub fn is_device_lost(&self) -> bool {
        self.errors.device_lost.load(Ordering::SeqCst) == DEVICE_LOST
    }

    /// Drain typed errors captured from the wgpu error handler since the
    /// last drain. The first error is the one to surface to the caller.
    pub fn take_uncaptured_errors(&self) -> Option<GpuError> {
        let mut uncaptured = self.errors.uncaptured.lock().ok()?;
        if uncaptured.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut *uncaptured).remove(0))
        }
    }
}
