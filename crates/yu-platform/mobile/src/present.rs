//! Draws the engine's screen texture into the host's view.
//!
//! The view is a UIView backed by a CAMetalLayer. wgpu makes a surface on
//! it from the same device the engine renders with, so the frame never
//! leaves the GPU: a quad samples the screen texture, fitted to the view
//! with black bars (`present.wgsl`, shared with the Android host's shader).

use std::ffi::c_void;

use yu_backend_wgpu::GpuContext;

pub struct Presenter {
    surface: wgpu::Surface<'static>,
    device: wgpu::Device,
    queue: wgpu::Queue,
    config: wgpu::SurfaceConfiguration,
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    sampler: wgpu::Sampler,
    params: wgpu::Buffer,
    bind_group: Option<wgpu::BindGroup>,
    /// Screen texture id the bind group samples.
    bound: Option<u64>,
    /// The view the surface was made for.
    pub view: *mut c_void,
}

#[cfg(any(target_os = "ios", target_os = "tvos", target_os = "visionos"))]
fn surface_target(view: *mut c_void) -> Option<wgpu::SurfaceTargetUnsafe> {
    use raw_window_handle::{RawDisplayHandle, RawWindowHandle, UiKitDisplayHandle, UiKitWindowHandle};
    let view = std::ptr::NonNull::new(view)?;
    Some(wgpu::SurfaceTargetUnsafe::RawHandle {
        raw_display_handle: Some(RawDisplayHandle::UiKit(UiKitDisplayHandle::new())),
        raw_window_handle: RawWindowHandle::UiKit(UiKitWindowHandle::new(view)),
    })
}

#[cfg(target_os = "macos")]
fn surface_target(view: *mut c_void) -> Option<wgpu::SurfaceTargetUnsafe> {
    use raw_window_handle::{AppKitDisplayHandle, AppKitWindowHandle, RawDisplayHandle, RawWindowHandle};
    let view = std::ptr::NonNull::new(view)?;
    Some(wgpu::SurfaceTargetUnsafe::RawHandle {
        raw_display_handle: Some(RawDisplayHandle::AppKit(AppKitDisplayHandle::new())),
        raw_window_handle: RawWindowHandle::AppKit(AppKitWindowHandle::new(view)),
    })
}

#[cfg(not(target_vendor = "apple"))]
fn surface_target(_view: *mut c_void) -> Option<wgpu::SurfaceTargetUnsafe> {
    None
}

/// A surface format without sRGB conversion: the engine's colours are
/// already sRGB values, as on Windows.
fn pick_format(caps: &wgpu::SurfaceCapabilities) -> Option<wgpu::TextureFormat> {
    caps.formats
        .iter()
        .copied()
        .find(|f| matches!(f, wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm))
        .or_else(|| caps.formats.iter().copied().find(|f| !f.is_srgb()))
        .or_else(|| caps.formats.first().copied())
}

impl Presenter {
    /// A surface on `view` of `width` x `height` pixels.
    ///
    /// # Safety
    /// `view` is a live UIView whose layer is a CAMetalLayer, and it
    /// outlives the presenter.
    pub unsafe fn new(gpu: &GpuContext, view: *mut c_void, width: u32, height: u32) -> Result<Self, String> {
        let target = surface_target(view).ok_or("views are not supported on this platform")?;
        let surface = gpu.instance.create_surface_unsafe(target).map_err(|e| e.to_string())?;
        let caps = surface.get_capabilities(&gpu.adapter);
        let format = pick_format(&caps).ok_or("the GPU cannot draw into this view")?;
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: width.max(1),
            height: height.max(1),
            present_mode: wgpu::PresentMode::Fifo,
            desired_maximum_frame_latency: 2,
            alpha_mode: if caps.alpha_modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
                wgpu::CompositeAlphaMode::Opaque
            } else {
                caps.alpha_modes[0]
            },
            view_formats: vec![],
        };
        surface.configure(&gpu.device, &config);

        let device = gpu.device.clone();
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("yuris present"),
            source: wgpu::ShaderSource::Wgsl(include_str!("present.wgsl").into()),
        });
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("yuris present bgl"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: true },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 2,
                    visibility: wgpu::ShaderStages::VERTEX,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("yuris present layout"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("yuris present pipeline"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                buffers: &[],
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs"),
                compilation_options: wgpu::PipelineCompilationOptions::default(),
                targets: &[Some(wgpu::ColorTargetState {
                    format,
                    blend: Some(wgpu::BlendState::REPLACE),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("yuris present sampler"),
            address_mode_u: wgpu::AddressMode::ClampToEdge,
            address_mode_v: wgpu::AddressMode::ClampToEdge,
            address_mode_w: wgpu::AddressMode::ClampToEdge,
            mag_filter: wgpu::FilterMode::Linear,
            min_filter: wgpu::FilterMode::Linear,
            mipmap_filter: wgpu::MipmapFilterMode::Nearest,
            ..Default::default()
        });
        let params = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("yuris present params"),
            size: 16,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Ok(Self {
            surface,
            device,
            queue: gpu.queue.clone(),
            config,
            pipeline,
            layout,
            sampler,
            params,
            bind_group: None,
            bound: None,
            view,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let (width, height) = (width.max(1), height.max(1));
        if (self.config.width, self.config.height) != (width, height) {
            self.config.width = width;
            self.config.height = height;
            self.surface.configure(&self.device, &self.config);
        }
    }

    pub fn size(&self) -> (u32, u32) {
        (self.config.width, self.config.height)
    }

    /// Draws the screen texture `id` (`width` x `height`) fitted into the view.
    pub fn present(&mut self, texture: &wgpu::Texture, id: u64, width: u32, height: u32) -> Result<(), String> {
        if self.bound != Some(id) || self.bind_group.is_none() {
            let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
            self.bind_group = Some(self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("yuris present bind group"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry { binding: 0, resource: wgpu::BindingResource::TextureView(&view) },
                    wgpu::BindGroupEntry { binding: 1, resource: wgpu::BindingResource::Sampler(&self.sampler) },
                    wgpu::BindGroupEntry { binding: 2, resource: self.params.as_entire_binding() },
                ],
            }));
            self.bound = Some(id);
        }
        let params: [f32; 4] = [
            width.max(1) as f32,
            height.max(1) as f32,
            self.config.width.max(1) as f32,
            self.config.height.max(1) as f32,
        ];
        self.queue.write_buffer(&self.params, 0, bytemuck::cast_slice(&params));

        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                // Drawn on the next frame, after the reconfigure.
                self.surface.configure(&self.device, &self.config);
                return Ok(());
            }
            other => return Err(format!("no drawable: {other:?}")),
        };
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("yuris present") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("yuris present pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations { load: wgpu::LoadOp::Clear(wgpu::Color::BLACK), store: wgpu::StoreOp::Store },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            if let Some(bind_group) = &self.bind_group {
                pass.set_bind_group(0, bind_group, &[]);
                pass.draw(0..6, 0..1);
            }
        }
        self.queue.submit(std::iter::once(encoder.finish()));
        self.queue.present(frame);
        Ok(())
    }
}
