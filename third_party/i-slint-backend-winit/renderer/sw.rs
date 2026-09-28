// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: GPL-3.0-only OR LicenseRef-Slint-Royalty-free-2.0 OR LicenseRef-Slint-Software-3.0

//! Delegate the rendering to the [`i_slint_renderer_software::SoftwareRenderer`]

use core::num::NonZeroU32;
use core::ops::DerefMut;
use i_slint_core::graphics::Rgb8Pixel;
use i_slint_core::platform::PlatformError;
use i_slint_core::renderer::DrawOutcome;
pub use i_slint_renderer_software::SoftwareRenderer;
use i_slint_renderer_software::{PremultipliedRgbaColor, RepaintBufferType, TargetPixel};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;
use winit::event_loop::ActiveEventLoop;

use super::WinitCompatibleRenderer;

pub struct WinitSoftwareRenderer {
    renderer: SoftwareRenderer,
    _context: RefCell<Option<softbuffer::Context<Arc<winit::window::Window>>>>,
    // PASTEZO PATCH: shared with `idle_release`
    surface: Rc<
        RefCell<
            Option<softbuffer::Surface<Arc<winit::window::Window>, Arc<winit::window::Window>>>,
        >,
    >,
    /// PASTEZO PATCH: 1 s after the last frame, frees the frame buffers not on
    /// screen (macOS otherwise keeps up to three, 10 MB each on Retina); while
    /// the window server still holds one, again every second (a few times).
    idle_release: Rc<i_slint_core::timers::Timer>,
}

#[repr(transparent)]
#[derive(Copy, Clone, bytemuck::Pod, bytemuck::Zeroable)]
struct SoftBufferPixel(pub u32);

impl From<SoftBufferPixel> for PremultipliedRgbaColor {
    #[inline]
    fn from(pixel: SoftBufferPixel) -> Self {
        let v = pixel.0;
        PremultipliedRgbaColor {
            red: (v >> 16) as u8,
            green: (v >> 8) as u8,
            blue: v as u8,
            alpha: (v >> 24) as u8,
        }
    }
}

impl From<PremultipliedRgbaColor> for SoftBufferPixel {
    #[inline]
    fn from(pixel: PremultipliedRgbaColor) -> Self {
        Self(
            (pixel.alpha as u32) << 24
                | ((pixel.red as u32) << 16)
                | ((pixel.green as u32) << 8)
                | (pixel.blue as u32),
        )
    }
}

// PASTEZO PATCH: blending on the packed pixel, two channels per multiply (the
// hottest code while scrolling). Same results as PremultipliedRgbaColor::blend,
// bit for bit: `(v + 1 + (v >> 8)) >> 8` is `v / 255` for every product of two bytes.
#[inline(always)]
fn blend_packed(dst: u32, src: u32, alpha: u32) -> u32 {
    let inv = 255 - alpha;
    let div255 = |v: u32| ((v + 0x0001_0001 + ((v >> 8) & 0x00ff_00ff)) >> 8) & 0x00ff_00ff;
    let rb = div255((dst & 0x00ff_00ff) * inv);
    let g = div255(((dst >> 8) & 0xff) * inv) << 8;
    let da = dst >> 24;
    let a = da + alpha - da * alpha / 255;
    ((rb | g) + (src & 0x00ff_ffff)) | (a << 24)
}

impl TargetPixel for SoftBufferPixel {
    #[inline]
    fn blend(&mut self, color: PremultipliedRgbaColor) {
        match color.alpha {
            0 => {}
            255 => *self = color.into(),
            a => self.0 = blend_packed(self.0, SoftBufferPixel::from(color).0, a as u32),
        }
    }

    fn blend_slice(slice: &mut [Self], color: PremultipliedRgbaColor) {
        match color.alpha {
            0 => {}
            255 => slice.fill(color.into()),
            a => {
                let src = SoftBufferPixel::from(color).0;
                for p in slice {
                    p.0 = blend_packed(p.0, src, a as u32);
                }
            }
        }
    }

    fn from_rgb(r: u8, g: u8, b: u8) -> Self {
        Self(0xff000000 | ((r as u32) << 16) | ((g as u32) << 8) | (b as u32))
    }

    fn background() -> Self {
        Self(0)
    }
}

impl WinitSoftwareRenderer {
    pub fn new_suspended(
        _shared_backend_data: &Rc<crate::SharedBackendData>,
    ) -> Result<Box<dyn WinitCompatibleRenderer>, PlatformError> {
        Ok(Box::new(Self {
            renderer: SoftwareRenderer::new(),
            _context: RefCell::new(None),
            surface: Default::default(),
            idle_release: Default::default(),
        }))
    }
}

impl super::WinitCompatibleRenderer for WinitSoftwareRenderer {
    fn render(&self, window: &i_slint_core::api::Window) -> Result<DrawOutcome, PlatformError> {
        let size = window.size();

        let Some((width, height)) = size.width.try_into().ok().zip(size.height.try_into().ok())
        else {
            // Nothing to render
            return Ok(DrawOutcome::Success);
        };

        let mut borrowed_surface = self.surface.borrow_mut();
        let Some(surface) = borrowed_surface.as_mut() else {
            // Nothing to render
            return Ok(DrawOutcome::Success);
        };

        let winit_window = surface.window().clone();

        surface
            .resize(width, height)
            .map_err(|e| format!("Error resizing softbuffer surface: {e}"))?;

        let mut target_buffer = surface
            .buffer_mut()
            .map_err(|e| format!("Error retrieving softbuffer rendering buffer: {e}"))?;

        let age = target_buffer.age();
        self.renderer.set_repaint_buffer_type(match age {
            1 => RepaintBufferType::ReusedBuffer,
            2 => RepaintBufferType::SwappedBuffers,
            _ => RepaintBufferType::NewBuffer,
        });

        // PASTEZO PATCH: looked up once, not on every frame
        static LINE_BY_LINE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        let region = if !*LINE_BY_LINE.get_or_init(|| std::env::var_os("SLINT_LINE_BY_LINE").is_some()) {
            // PASTEZO PATCH: the patched softbuffer (macOS, IOSurface) may pad
            // rows; its buffer is `stride * height` long. Elsewhere stride == width.
            let stride = target_buffer.len() / height.get() as usize;
            let buffer: &mut [SoftBufferPixel] =
                bytemuck::cast_slice_mut(target_buffer.deref_mut());
            self.renderer.render(buffer, stride)
        } else {
            // SLINT_LINE_BY_LINE is set and this is a debug mode where we also render in a Rgb565Pixel
            struct FrameBuffer<'a> {
                buffer: &'a mut [u32],
                line: Vec<i_slint_renderer_software::Rgb565Pixel>,
            }
            impl i_slint_renderer_software::LineBufferProvider for FrameBuffer<'_> {
                type TargetPixel = i_slint_renderer_software::Rgb565Pixel;
                fn process_line(
                    &mut self,
                    line: usize,
                    range: core::ops::Range<usize>,
                    render_fn: impl FnOnce(&mut [Self::TargetPixel]),
                ) {
                    let line_begin = line * self.line.len();
                    let sub = &mut self.line[..range.len()];
                    render_fn(sub);
                    for (dst, src) in self.buffer[line_begin..][range].iter_mut().zip(sub) {
                        let p = Rgb8Pixel::from(*src);
                        *dst =
                            0xff000000 | ((p.r as u32) << 16) | ((p.g as u32) << 8) | (p.b as u32);
                    }
                }
            }
            self.renderer.render_by_line(FrameBuffer {
                buffer: &mut target_buffer,
                line: vec![Default::default(); width.get() as usize],
            })
        };

        let damage = region
            .iter()
            .filter_map(|(pos, size)| {
                Some(softbuffer::Rect {
                    x: pos.x as u32,
                    y: pos.y as u32,
                    width: NonZeroU32::new(size.width)?,
                    height: NonZeroU32::new(size.height)?,
                })
            })
            .collect::<Vec<_>>();
        if !damage.is_empty() {
            winit_window.pre_present_notify();
            target_buffer
                .present_with_damage(&damage)
                .map_err(|e| format!("Error presenting softbuffer buffer: {e}"))?;
            let surface = Rc::downgrade(&self.surface);
            let timer = Rc::downgrade(&self.idle_release);
            let tries = core::cell::Cell::new(10u8);
            self.idle_release.start(
                i_slint_core::timers::TimerMode::SingleShot,
                core::time::Duration::from_secs(1),
                move || {
                    let Some(s) = surface.upgrade() else { return };
                    let done = s.borrow_mut().as_mut().map_or(true, |s| s.release_spare());
                    tries.set(tries.get().saturating_sub(1));
                    if !done && tries.get() > 0 {
                        if let Some(t) = timer.upgrade() {
                            t.restart();
                        }
                    }
                },
            );
        }
        Ok(DrawOutcome::Success)
    }

    fn as_core_renderer(&self) -> &dyn i_slint_core::renderer::Renderer {
        &self.renderer
    }

    fn occluded(&self, _: bool) {
        // On X11 and Windows, the buffer is completely cleared when the window is hidden
        // and the buffer age doesn't respect that, so clean the partial rendering cache
        self.renderer.set_repaint_buffer_type(RepaintBufferType::NewBuffer);
    }

    fn resume(
        &self,
        active_event_loop: &ActiveEventLoop,
        window_attributes: winit::window::WindowAttributes,
        _window_adapter_weak: std::rc::Weak<crate::winitwindowadapter::WinitWindowAdapter>,
    ) -> Result<Arc<winit::window::Window>, PlatformError> {
        let winit_window =
            active_event_loop.create_window(window_attributes).map_err(|winit_os_error| {
                PlatformError::from(format!(
                    "Error creating native window for software rendering: {winit_os_error}"
                ))
            })?;
        let winit_window = Arc::new(winit_window);

        let context = softbuffer::Context::new(winit_window.clone())
            .map_err(|e| format!("Error creating softbuffer context: {e}"))?;

        let surface = softbuffer::Surface::new(&context, winit_window.clone()).map_err(
            |softbuffer_error| format!("Error creating softbuffer surface: {softbuffer_error}"),
        )?;

        *self._context.borrow_mut() = Some(context);
        *self.surface.borrow_mut() = Some(surface);

        Ok(winit_window)
    }

    fn suspend(&self) -> Result<(), PlatformError> {
        drop(self.surface.borrow_mut().take());
        drop(self._context.borrow_mut().take());
        Ok(())
    }
}
