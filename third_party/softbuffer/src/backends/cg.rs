//! Softbuffer implementation using CoreGraphics.
//!
//! PASTEZO PATCH: frames are presented through persistent IOSurfaces set as
//! the layer contents, instead of a new heap buffer per frame wrapped in a
//! CGImage (which CoreAnimation then copied). IOSurface memory is shared with
//! the window server, so there is no copy. Up to three surfaces take turns: the
//! window server may still be compositing the previous one (fast scrolling),
//! and a third keeps us from allocating 10 MB and redrawing everything then.
//! Surfaces not on screen are freed by `release_spare` once the window is
//! idle, so it keeps one frame of pixels. They are not marked purgeable: the
//! system empties such a surface at once, also while the window server still
//! shows it (a black flash when scrolling), and every frame is then redrawn.
//! Buffer rows may be padded (IOSurface alignment): `pixels()` is
//! `stride * height` long, the patched Slint backend derives the stride from it.
use crate::backend_interface::*;
use crate::error::InitError;
use crate::{Rect, SoftBufferError};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass, MainThreadMarker, Message};
use objc2_core_foundation::{CFDictionary, CFNumber, CFRetained, CFString, CGPoint};
use objc2_io_surface::{
    kIOSurfaceBytesPerElement, kIOSurfaceBytesPerRow, kIOSurfaceHeight, kIOSurfacePixelFormat,
    kIOSurfaceWidth, IOSurfaceLockOptions, IOSurfaceRef,
};
use objc2_foundation::{
    ns_string, NSDictionary, NSKeyValueChangeKey, NSKeyValueChangeNewKey,
    NSKeyValueObservingOptions, NSNumber, NSObject, NSObjectNSKeyValueObserverRegistration,
    NSString, NSValue,
};
use objc2_quartz_core::{kCAGravityTopLeft, CALayer, CATransaction};
use raw_window_handle::{HasDisplayHandle, HasWindowHandle, RawWindowHandle};

use std::ffi::c_void;
use std::marker::PhantomData;
use std::num::NonZeroU32;
use std::ops::Deref;
use std::ptr::{self, slice_from_raw_parts_mut, NonNull};

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "SoftbufferObserver"]
    #[ivars = SendCALayer]
    #[derive(Debug)]
    struct Observer;

    /// NSKeyValueObserving
    impl Observer {
        #[unsafe(method(observeValueForKeyPath:ofObject:change:context:))]
        fn observe_value(
            &self,
            key_path: Option<&NSString>,
            _object: Option<&AnyObject>,
            change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
            _context: *mut c_void,
        ) {
            self.update(key_path, change);
        }
    }
);

impl Observer {
    fn new(layer: &CALayer) -> Retained<Self> {
        let this = Self::alloc().set_ivars(SendCALayer(layer.retain()));
        unsafe { msg_send![super(this), init] }
    }

    fn update(
        &self,
        key_path: Option<&NSString>,
        change: Option<&NSDictionary<NSKeyValueChangeKey, AnyObject>>,
    ) {
        let layer = self.ivars();

        let change =
            change.expect("requested a change dictionary in `addObserver`, but none was provided");
        let new = change
            .objectForKey(unsafe { NSKeyValueChangeNewKey })
            .expect("requested change dictionary did not contain `NSKeyValueChangeNewKey`");

        // NOTE: Setting these values usually causes a quarter second animation to occur, which is
        // undesirable.
        //
        // However, since we're setting them inside an observer, there already is a transaction
        // ongoing, and as such we don't need to wrap this in a `CATransaction` ourselves.

        if key_path == Some(ns_string!("contentsScale")) {
            let new = new.downcast::<NSNumber>().unwrap();
            let scale_factor = new.as_cgfloat();

            // Set the scale factor of the layer to match the root layer when it changes (e.g. if
            // moved to a different monitor, or monitor settings changed).
            layer.setContentsScale(scale_factor);
        } else if key_path == Some(ns_string!("bounds")) {
            let new = new.downcast::<NSValue>().unwrap();
            let bounds = new.get_rect().expect("new bounds value was not CGRect");

            // Set `bounds` and `position` so that the new layer is inside the superlayer.
            //
            // This differs from just setting the `bounds`, as it also takes into account any
            // translation that the superlayer may have that we'd want to preserve.
            layer.setFrame(bounds);
        } else {
            panic!("unknown observed keypath {key_path:?}");
        }
    }
}

/// Surfaces in turn: shown, being composited, being drawn into.
const SURFACES: usize = 3;

#[derive(Debug)]
pub struct CGImpl<D, W> {
    /// Our layer.
    layer: SendCALayer,
    /// The layer that our layer was created from.
    ///
    /// Can also be retrieved from `layer.superlayer()`.
    root_layer: SendCALayer,
    observer: Retained<Observer>,
    /// Surfaces taking turns between being shown and being drawn into.
    surfaces: [Option<Surface>; SURFACES],
    /// Index of the surface currently set as the layer contents.
    front: Option<usize>,
    /// The width of the underlying buffer.
    width: usize,
    /// The height of the underlying buffer.
    height: usize,
    window_handle: W,
    _display: PhantomData<D>,
}

impl<D, W> Drop for CGImpl<D, W> {
    fn drop(&mut self) {
        // SAFETY: Registered in `new`, must be removed before the observer is deallocated.
        unsafe {
            self.root_layer
                .removeObserver_forKeyPath(&self.observer, ns_string!("contentsScale"));
            self.root_layer
                .removeObserver_forKeyPath(&self.observer, ns_string!("bounds"));
        }
    }
}

impl<D: HasDisplayHandle, W: HasWindowHandle> SurfaceInterface<D, W> for CGImpl<D, W> {
    type Context = D;
    type Buffer<'a>
        = BufferImpl<'a, D, W>
    where
        Self: 'a;

    fn new(window_src: W, _display: &D) -> Result<Self, InitError<W>> {
        // `NSView`/`UIView` can only be accessed from the main thread.
        let _mtm = MainThreadMarker::new().ok_or(SoftBufferError::PlatformError(
            Some("can only access Core Graphics handles from the main thread".to_string()),
            None,
        ))?;

        let root_layer = match window_src.window_handle()?.as_raw() {
            RawWindowHandle::AppKit(handle) => {
                // SAFETY: The pointer came from `WindowHandle`, which ensures that the
                // `AppKitWindowHandle` contains a valid pointer to an `NSView`.
                //
                // We use `NSObject` here to avoid importing `objc2-app-kit`.
                let view: &NSObject = unsafe { handle.ns_view.cast().as_ref() };

                // Force the view to become layer backed
                let _: () = unsafe { msg_send![view, setWantsLayer: Bool::YES] };

                // SAFETY: `-[NSView layer]` returns an optional `CALayer`
                let layer: Option<Retained<CALayer>> = unsafe { msg_send![view, layer] };
                layer.expect("failed making the view layer-backed")
            }
            RawWindowHandle::UiKit(handle) => {
                // SAFETY: The pointer came from `WindowHandle`, which ensures that the
                // `UiKitWindowHandle` contains a valid pointer to an `UIView`.
                //
                // We use `NSObject` here to avoid importing `objc2-ui-kit`.
                let view: &NSObject = unsafe { handle.ui_view.cast().as_ref() };

                // SAFETY: `-[UIView layer]` returns `CALayer`
                let layer: Retained<CALayer> = unsafe { msg_send![view, layer] };
                layer
            }
            _ => return Err(InitError::Unsupported(window_src)),
        };

        // Add a sublayer, to avoid interfering with the root layer, since setting the contents of
        // e.g. a view-controlled layer is brittle.
        let layer = CALayer::new();
        root_layer.addSublayer(&layer);

        // Set the anchor point and geometry. Softbuffer's uses a coordinate system with the origin
        // in the top-left corner.
        //
        // NOTE: This doesn't really matter unless we start modifying the `position` of our layer
        // ourselves, but it's nice to have in place.
        layer.setAnchorPoint(CGPoint::new(0.0, 0.0));
        layer.setGeometryFlipped(true);

        // Do not use auto-resizing mask.
        //
        // This is done to work around a bug in macOS 14 and above, where views using auto layout
        // may end up setting fractional values as the bounds, and that in turn doesn't propagate
        // properly through the auto-resizing mask and with contents gravity.
        //
        // Instead, we keep the bounds of the layer in sync with the root layer using an observer,
        // see below.
        //
        // layer.setAutoresizingMask(kCALayerHeightSizable | kCALayerWidthSizable);

        let observer = Observer::new(&layer);
        // Observe changes to the root layer's bounds and scale factor, and apply them to our layer.
        //
        // The previous implementation updated the scale factor inside `resize`, but this works
        // poorly with transactions, and is generally inefficient. Instead, we update the scale
        // factor only when needed because the super layer's scale factor changed.
        //
        // Note that inherent in this is an explicit design decision: We control the `bounds` and
        // `contentsScale` of the layer directly, and instead let the `resize` call that the user
        // controls only be the size of the underlying buffer.
        //
        // SAFETY: Observer deregistered in `Drop` before the observer object is deallocated.
        unsafe {
            root_layer.addObserver_forKeyPath_options_context(
                &observer,
                ns_string!("contentsScale"),
                NSKeyValueObservingOptions::New | NSKeyValueObservingOptions::Initial,
                ptr::null_mut(),
            );
            root_layer.addObserver_forKeyPath_options_context(
                &observer,
                ns_string!("bounds"),
                NSKeyValueObservingOptions::New | NSKeyValueObservingOptions::Initial,
                ptr::null_mut(),
            );
        }

        // Set the content so that it is placed in the top-left corner if it does not have the same
        // size as the surface itself.
        //
        // TODO(madsmtm): Consider changing this to `kCAGravityResize` to stretch the content if
        // resized to something that doesn't fit, see #177.
        layer.setContentsGravity(unsafe { kCAGravityTopLeft });

        // Every pixel is painted by the app, so the window server can skip blending.
        layer.setOpaque(true);

        // Grab initial width and height from the layer (whose properties have just been initialized
        // by the observer using `NSKeyValueObservingOptionInitial`).
        let size = layer.bounds().size;
        let scale_factor = layer.contentsScale();
        let width = (size.width * scale_factor) as usize;
        let height = (size.height * scale_factor) as usize;

        Ok(Self {
            layer: SendCALayer(layer),
            root_layer: SendCALayer(root_layer),
            observer,
            surfaces: [None, None, None],
            front: None,
            width,
            height,
            _display: PhantomData,
            window_handle: window_src,
        })
    }

    #[inline]
    fn window(&self) -> &W {
        &self.window_handle
    }

    fn resize(&mut self, width: NonZeroU32, height: NonZeroU32) -> Result<(), SoftBufferError> {
        self.width = width.get() as usize;
        self.height = height.get() as usize;
        Ok(())
    }

    fn release_spare(&mut self) -> bool {
        let mut done = true;
        for (i, s) in self.surfaces.iter_mut().enumerate() {
            if Some(i) == self.front || s.is_none() {
                continue;
            }
            // the window server may still hold one it just stopped showing
            // (e.g. after the window changed key status): later
            if s.as_ref().is_some_and(|s| s.io.is_in_use()) {
                done = false;
            } else {
                *s = None;
            }
        }
        done
    }

    fn buffer_mut(&mut self) -> Result<BufferImpl<'_, D, W>, SoftBufferError> {
        let (width, height) = (self.width, self.height);
        let fits = |s: &Surface| s.io.width() == width && s.io.height() == height;
        // new size: the old surfaces are useless (the layer keeps showing the
        // current contents until the next present)
        if self.surfaces.iter().flatten().any(|s| !fits(s)) {
            self.surfaces = [None, None, None];
            self.front = None;
        }
        // Draw into a surface that is not on screen and that the window server
        // is done with (drawing into one still being composited could tear),
        // the one with the most recent contents: less to redraw.
        let free = (0..SURFACES)
            .filter(|&i| Some(i) != self.front)
            .filter(|&i| self.surfaces[i].as_ref().is_some_and(|s| !s.io.is_in_use()))
            .min_by_key(|&i| match self.surfaces[i].as_ref().unwrap().age {
                0 => u8::MAX,
                age => age,
            });
        let index = match free {
            Some(i) => i,
            None => {
                // all busy: a new one in an empty slot, or in place of the oldest
                // (the window server keeps its own reference while it needs it)
                let i = (0..SURFACES)
                    .filter(|&i| Some(i) != self.front)
                    .min_by_key(|&i| self.surfaces[i].as_ref().map_or(0, |s| u16::from(u8::MAX - s.age) + 1))
                    .unwrap();
                self.surfaces[i] = Some(Surface::new(width, height)?);
                i
            }
        };
        let surface = self.surfaces[index].as_mut().unwrap();
        if unsafe { surface.io.lock(IOSurfaceLockOptions::empty(), ptr::null_mut()) } != 0 {
            return Err(SoftBufferError::PlatformError(Some("IOSurfaceLock failed".into()), None));
        }
        let len = surface.stride * height;
        let pixels = surface.io.base_address().cast::<u32>();
        Ok(BufferImpl { imp: self, index, pixels: SendPixels(pixels), len, locked: true })
    }
}

/// IOSurface objects are thread-safe (IOSurface.framework), so they may be
/// sent and shared like the rest of softbuffer's surface state.
#[derive(Debug)]
struct SendIOSurface(CFRetained<IOSurfaceRef>);
unsafe impl Send for SendIOSurface {}
unsafe impl Sync for SendIOSurface {}

impl Deref for SendIOSurface {
    type Target = IOSurfaceRef;
    fn deref(&self) -> &IOSurfaceRef {
        &self.0
    }
}

/// Pointer into a locked IOSurface; valid while the owning buffer lives.
#[derive(Debug)]
struct SendPixels(NonNull<u32>);
unsafe impl Send for SendPixels {}
unsafe impl Sync for SendPixels {}

/// One frame of pixels shared with the window server.
#[derive(Debug)]
struct Surface {
    io: SendIOSurface,
    /// Pixels per row (rows may be padded for alignment).
    stride: usize,
    /// Frames since this surface was shown (0: contents undefined).
    age: u8,
}

impl Surface {
    fn new(width: usize, height: usize) -> Result<Self, SoftBufferError> {
        let bytes_per_row = IOSurfaceRef::align_property(unsafe { kIOSurfaceBytesPerRow }, width * 4);
        // 'BGRA': the byte order of softbuffer's 0xAARRGGBB pixels in memory
        const BGRA: i32 = 0x4247_5241;
        let keys: [&CFString; 5] = unsafe {
            [
                kIOSurfaceWidth,
                kIOSurfaceHeight,
                kIOSurfaceBytesPerElement,
                kIOSurfaceBytesPerRow,
                kIOSurfacePixelFormat,
            ]
        };
        let values = [
            CFNumber::new_isize(width as isize),
            CFNumber::new_isize(height as isize),
            CFNumber::new_i32(4),
            CFNumber::new_isize(bytes_per_row as isize),
            CFNumber::new_i32(BGRA),
        ];
        let values: Vec<&CFNumber> = values.iter().map(|v| &**v).collect();
        let props = CFDictionary::from_slices(&keys, &values);
        let io = unsafe { IOSurfaceRef::new(props.as_opaque()) }
            .ok_or_else(|| SoftBufferError::PlatformError(Some("IOSurfaceCreate failed".into()), None))?;
        let stride = io.bytes_per_row() / 4;
        Ok(Self { io: SendIOSurface(io), stride, age: 0 })
    }
}

#[derive(Debug)]
pub struct BufferImpl<'a, D, W> {
    imp: &'a mut CGImpl<D, W>,
    index: usize,
    pixels: SendPixels,
    len: usize,
    /// Surfaces stay locked while being drawn into; unlocked by present() or drop.
    locked: bool,
}

impl<D: HasDisplayHandle, W: HasWindowHandle> BufferInterface for BufferImpl<'_, D, W> {
    fn width(&self) -> NonZeroU32 {
        NonZeroU32::new(self.imp.width as u32).unwrap()
    }

    fn height(&self) -> NonZeroU32 {
        NonZeroU32::new(self.imp.height as u32).unwrap()
    }

    #[inline]
    fn pixels(&self) -> &[u32] {
        // SAFETY: the surface is locked for the lifetime of this buffer
        unsafe { &*slice_from_raw_parts_mut(self.pixels.0.as_ptr(), self.len) }
    }

    #[inline]
    fn pixels_mut(&mut self) -> &mut [u32] {
        // SAFETY: the surface is locked for the lifetime of this buffer
        unsafe { &mut *slice_from_raw_parts_mut(self.pixels.0.as_ptr(), self.len) }
    }

    fn age(&self) -> u8 {
        self.imp.surfaces[self.index].as_ref().map_or(0, |s| s.age)
    }

    fn present(mut self) -> Result<(), SoftBufferError> {
        self.unlock();
        let index = self.index;
        let imp = &mut *self.imp;
        let surface = imp.surfaces[index].as_mut().unwrap();

        // The CALayer has a default action associated with a change in the layer contents, causing
        // a quarter second fade transition to happen every time a new buffer is applied. This can
        // be avoided by wrapping the operation in a transaction and disabling all actions.
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        // SAFETY: an IOSurface is a valid class for `contents`.
        let contents: &AnyObject = unsafe { &*(&*surface.io.0 as *const IOSurfaceRef).cast() };
        unsafe { imp.layer.setContents(Some(contents)) };
        CATransaction::commit();

        surface.age = 1;
        imp.front = Some(index);
        for (i, spare) in imp.surfaces.iter_mut().enumerate() {
            let Some(spare) = spare.as_mut().filter(|_| i != index) else { continue };
            if spare.age > 0 {
                spare.age = spare.age.saturating_add(1);
            }
        }
        Ok(())
    }

    fn present_with_damage(self, _damage: &[Rect]) -> Result<(), SoftBufferError> {
        self.present()
    }
}

impl<D, W> BufferImpl<'_, D, W> {
    fn unlock(&mut self) {
        if std::mem::take(&mut self.locked) {
            if let Some(s) = self.imp.surfaces[self.index].as_ref() {
                unsafe { s.io.unlock(IOSurfaceLockOptions::empty(), ptr::null_mut()) };
            }
        }
    }
}

impl<D, W> Drop for BufferImpl<'_, D, W> {
    fn drop(&mut self) {
        // dropped without present() (nothing changed): keep lock/unlock balanced
        self.unlock();
    }
}

#[derive(Debug)]
struct SendCALayer(Retained<CALayer>);

// SAFETY: CALayer is dubiously thread safe, like most things in Core Animation.
// But since we make sure to do our changes within a CATransaction, it is
// _probably_ fine for us to use CALayer from different threads.
//
// See also:
// https://developer.apple.com/documentation/quartzcore/catransaction/1448267-lock?language=objc
// https://stackoverflow.com/questions/76250226/how-to-render-content-of-calayer-on-a-background-thread
unsafe impl Send for SendCALayer {}
// SAFETY: Same as above.
unsafe impl Sync for SendCALayer {}

impl Deref for SendCALayer {
    type Target = CALayer;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
