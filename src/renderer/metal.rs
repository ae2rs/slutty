use std::cell::RefMut;
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;
use std::time::Instant;

use libghostty_vt::color::Rgb;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_core_foundation::CGSize;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlendFactor, MTLBuffer, MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue,
    MTLCreateSystemDefaultDevice, MTLDevice, MTLDrawable, MTLLibrary, MTLLoadAction, MTLOrigin, MTLPixelFormat,
    MTLPrimitiveType, MTLRegion, MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor,
    MTLRenderPipelineState, MTLResourceOptions, MTLSize, MTLStoreAction, MTLTexture, MTLTextureDescriptor,
    MTLTextureUsage,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer, kCAGravityTopLeft};

use super::atlas::{Atlas, Full, MAX_SIZE};
use super::scene::{Chrome, GlyphInstance, Line, Overlay, RectInstance, Scene};
use crate::font::{Fonts, Metrics};
use crate::mux::PaneId;
use crate::terminal::Frame;

pub const PIXEL_FORMAT: MTLPixelFormat = MTLPixelFormat::BGRA8Unorm;

#[derive(Debug)]
pub struct RenderError(String);

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RenderError {}

/// Vertex uniforms; the layout matches `Uniforms` in shaders.metal.
#[repr(C)]
#[derive(Clone, Copy)]
struct Uniforms {
    viewport: [f32; 2],
    origin: [f32; 2],
    tint: Tint,
}

type Device = ProtocolObject<dyn MTLDevice>;
type Pipeline = Retained<ProtocolObject<dyn MTLRenderPipelineState>>;
type Texture = ProtocolObject<dyn MTLTexture>;
type Commands = Retained<ProtocolObject<dyn MTLCommandBuffer>>;

/// Scales a pane's colors in HSV, as WezTerm's `inactive_pane_hsb` does.
#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tint {
    pub saturation: f32,
    pub brightness: f32,
}

impl Tint {
    pub const NONE: Self = Self { saturation: 1.0, brightness: 1.0 };
}

impl Default for Tint {
    fn default() -> Self {
        Self::NONE
    }
}

/// One pane to draw: its frame, where it goes, and what lies on top.
pub struct PaneDraw<'a> {
    pub id: PaneId,
    pub frame: RefMut<'a, Frame>,
    /// Top-left corner in device pixels.
    pub origin: [f32; 2],
    pub overlay: Overlay<'a>,
    pub tint: Tint,
    /// Rows not built by then wait for a later frame, and the frame stays
    /// changed. `None` builds them all.
    pub deadline: Option<Instant>,
}

pub struct Renderer {
    device: Retained<Device>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    rect_pipeline: Pipeline,
    glyph_pipeline: Pipeline,
    texture: Retained<ProtocolObject<dyn MTLTexture>>,
    fonts: Fonts,
    atlas: Atlas,
    /// The atlas started over, and no frame has been built whole since: if it
    /// fills again, one screen does not fit, and it grows.
    refilling: bool,
    /// The panes drawn last time. Others are dropped and built from
    /// scratch when they show again.
    scenes: HashMap<PaneId, Scene>,
    chrome: Chrome,
}

impl Renderer {
    pub fn new(fonts: Fonts) -> Result<Self, RenderError> {
        let device = MTLCreateSystemDefaultDevice().ok_or_else(|| err("no Metal device"))?;
        let queue = device.newCommandQueue().ok_or_else(|| err("no command queue"))?;
        let source = NSString::from_str(include_str!("shaders.metal"));
        let library = device
            .newLibraryWithSource_options_error(&source, None)
            .map_err(|e| err(&format!("shaders: {}", e.localizedDescription())))?;
        let rect_pipeline = pipeline(&device, &library, "rect_vertex", "rect_fragment")?;
        let glyph_pipeline = pipeline(&device, &library, "glyph_vertex", "glyph_fragment")?;

        let atlas = Atlas::default();
        let texture = atlas_texture(&device, atlas.size()).ok_or_else(|| err("no atlas texture"))?;

        Ok(Self {
            device,
            queue,
            rect_pipeline,
            glyph_pipeline,
            texture,
            fonts,
            atlas,
            refilling: false,
            scenes: HashMap::new(),
            chrome: Chrome::default(),
        })
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    pub fn metrics(&self) -> Metrics {
        self.fonts.metrics()
    }

    /// Swaps the fonts, after a change of size or display scale.
    pub fn set_fonts(&mut self, fonts: Fonts) {
        self.fonts = fonts;
        self.atlas.reset();
        self.scenes.clear();
    }

    /// Draws the panes into `surface` over `clear` (transparent when `None`),
    /// then `rects` (device pixels from the top-left), then the `chrome`
    /// lines, leaving out what `shown` hides. The surface keeps the
    /// instances, so [`Renderer::redraw`] can draw them again.
    pub fn render(
        &mut self,
        surface: &mut Surface,
        panes: &mut [PaneDraw<'_>],
        rects: &[RectInstance],
        chrome: &[Line],
        clear: Option<Rgb>,
        shown: Shown,
    ) {
        surface.packed = Some(self.prepare(surface.layer.drawableSize(), panes, rects, chrome, clear));
        self.present(surface, shown);
    }

    /// Draws what `surface` showed last again with `shown`, without building
    /// it. Returns false when it has to be rendered instead: nothing was
    /// drawn yet, or the glyph atlas started over since.
    pub fn redraw(&self, surface: &Surface, shown: Shown) -> bool {
        if surface.packed.as_ref().is_none_or(|p| p.atlas != self.atlas.generation()) {
            return false;
        }
        self.present(surface, shown);
        true
    }

    fn present(&self, surface: &Surface, shown: Shown) {
        let Some(packed) = &surface.packed else { return };
        let Some(drawable) = surface.layer.nextDrawable() else {
            log::warn!("no drawable; skipping a frame");
            return;
        };
        match self.encode(&drawable.texture(), packed, shown) {
            Ok(commands) => {
                commands.presentDrawable(ProtocolObject::<dyn MTLDrawable>::from_ref(&*drawable));
                commands.commit();
            }
            Err(e) => log::error!("drawing failed: {e}"),
        }
    }

    /// Builds the instances for a target of `size` pixels and copies them
    /// into one buffer.
    fn prepare(
        &mut self,
        size: CGSize,
        panes: &mut [PaneDraw<'_>],
        rects: &[RectInstance],
        chrome: &[Line],
        clear: Option<Rgb>,
    ) -> Packed {
        self.scenes.retain(|id, _| panes.iter().any(|p| p.id == *id));
        while self.build(panes, chrome).is_err() {
            if !self.refilling {
                log::info!("glyph atlas full; starting over");
                self.atlas.reset();
                self.refilling = true;
            } else if !self.grow_atlas() {
                log::warn!("glyph atlas too small for one screen");
                break;
            }
            self.scenes.values_mut().for_each(Scene::invalidate);
        }
        if !panes.iter().any(|pane| pane.frame.changed) {
            self.refilling = false;
        }
        self.upload_glyphs();

        let viewport = [size.width as f32, size.height as f32];
        let mut batches = Vec::new();
        for pane in panes.iter() {
            let Some(s) = self.scenes.get(&pane.id) else { continue };
            let uniforms = Uniforms { viewport, origin: pane.origin, tint: pane.tint };
            let overlay = if pane.overlay.cursor_blinks(pane.frame.cursor) { Zone::Cursor } else { Zone::Always };
            batches.extend([
                (uniforms, Zone::Always, Batch::Rects(&s.rects)),
                (uniforms, Zone::Always, Batch::Glyphs(&s.glyphs)),
                (uniforms, Zone::Text, Batch::Rects(&s.blink_rects)),
                (uniforms, Zone::Text, Batch::Glyphs(&s.blink_glyphs)),
                (uniforms, overlay, Batch::Rects(&s.overlay_rects)),
                (uniforms, overlay, Batch::Glyphs(&s.overlay_glyphs)),
            ]);
        }
        let screen = Uniforms { viewport, origin: [0.0, 0.0], tint: Tint::NONE };
        batches.extend([
            (screen, Zone::Always, Batch::Rects(rects)),
            (screen, Zone::Always, Batch::Rects(&self.chrome.rects)),
            (screen, Zone::Always, Batch::Glyphs(&self.chrome.glyphs)),
        ]);
        pack(&self.device, &batches, clear, self.atlas.generation())
    }

    fn build(&mut self, panes: &mut [PaneDraw<'_>], chrome: &[Line]) -> Result<(), Full> {
        for pane in panes {
            let scene = self.scenes.entry(pane.id).or_default();
            scene.build(&mut pane.frame, &pane.overlay, &self.fonts, &mut self.atlas, pane.deadline)?;
        }
        self.chrome.build(chrome, &self.fonts, &mut self.atlas)
    }

    /// Doubles the atlas, emptied. Returns whether it grew: not past
    /// [`MAX_SIZE`], and only if Metal gave the texture.
    fn grow_atlas(&mut self) -> bool {
        let size = self.atlas.size() * 2;
        if size > MAX_SIZE {
            return false;
        }
        let Some(texture) = atlas_texture(&self.device, size) else {
            log::warn!("no {size}px glyph atlas texture");
            return false;
        };
        log::info!("glyph atlas grown to {size}px");
        self.texture = texture;
        self.atlas.reset_to(size);
        true
    }

    fn upload_glyphs(&mut self) {
        for upload in self.atlas.take_pending() {
            let g = upload.glyph;
            let region = MTLRegion {
                origin: MTLOrigin { x: g.x as usize, y: g.y as usize, z: 0 },
                size: MTLSize { width: g.width as usize, height: g.height as usize, depth: 1 },
            };
            let Some(bytes) = NonNull::new(upload.data.as_ptr().cast_mut().cast::<c_void>()) else { continue };
            // SAFETY: `data` holds `height` rows of `width * 4` bytes, and the
            // region lies inside the texture.
            unsafe {
                self.texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(region, 0, bytes, g.width as usize * 4)
            };
        }
    }

    /// Draws each batch `shown` lets through with its uniforms, in order.
    fn encode(&self, target: &Texture, packed: &Packed, shown: Shown) -> Result<Commands, RenderError> {
        let pass = MTLRenderPassDescriptor::new();
        // SAFETY: index 0 always exists.
        let color = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        color.setTexture(Some(target));
        color.setLoadAction(MTLLoadAction::Clear);
        color.setStoreAction(MTLStoreAction::Store);
        let c = |v: u8| f64::from(v) / 255.0;
        color.setClearColor(match packed.clear {
            Some(rgb) => MTLClearColor { red: c(rgb.r), green: c(rgb.g), blue: c(rgb.b), alpha: 1.0 },
            None => MTLClearColor { red: 0.0, green: 0.0, blue: 0.0, alpha: 0.0 },
        });

        let commands = self.queue.commandBuffer().ok_or_else(|| err("no command buffer"))?;
        let encoder = commands.renderCommandEncoderWithDescriptor(&pass).ok_or_else(|| err("no encoder"))?;
        if let Some(buffer) = &packed.buffer {
            // SAFETY: the uniforms outlive the calls, which copy them; every
            // offset and count describes instances inside `buffer`.
            unsafe {
                encoder.setFragmentTexture_atIndex(Some(&self.texture), 0);
                for batch in packed.batches.iter().filter(|b| b.count > 0 && shown.lets(b.zone)) {
                    let uniforms = NonNull::from(&batch.uniforms).cast();
                    encoder.setVertexBytes_length_atIndex(uniforms, size_of::<Uniforms>(), 0);
                    let pipeline = if batch.glyphs { &self.glyph_pipeline } else { &self.rect_pipeline };
                    encoder.setRenderPipelineState(pipeline);
                    encoder.setVertexBuffer_offset_atIndex(Some(buffer), batch.offset, 1);
                    encoder.drawPrimitives_vertexStart_vertexCount_instanceCount(
                        MTLPrimitiveType::TriangleStrip,
                        0,
                        4,
                        batch.count,
                    );
                }
            }
        }
        encoder.endEncoding();
        Ok(commands)
    }
}

/// A window's layer, and the instances last drawn into it.
pub struct Surface {
    layer: Retained<CAMetalLayer>,
    packed: Option<Packed>,
}

impl Surface {
    pub fn new(device: &Device, scale: f64) -> Self {
        let layer = CAMetalLayer::new();
        layer.setDevice(Some(device));
        layer.setPixelFormat(PIXEL_FORMAT);
        layer.setFramebufferOnly(true);
        layer.setContentsScale(scale);
        // A stale frame during a resize stays put instead of stretching.
        layer.setContentsGravity(unsafe { kCAGravityTopLeft });
        Self { layer, packed: None }
    }

    /// A surface whose layer shows what lies below it wherever nothing is
    /// drawn; render it with no clear color.
    pub fn transparent(device: &Device, scale: f64) -> Self {
        let surface = Self::new(device, scale);
        surface.layer.setOpaque(false);
        surface
    }

    pub fn layer(&self) -> &Retained<CAMetalLayer> {
        &self.layer
    }

    /// What was drawn last has text that blinks.
    pub fn blinks_text(&self) -> bool {
        self.packed.as_ref().is_some_and(|p| p.batches.iter().any(|b| b.zone == Zone::Text && b.count > 0))
    }
}

/// Which zones that blink are in their visible phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shown {
    pub cursor: bool,
    pub text: bool,
}

impl Shown {
    pub const ALL: Self = Self { cursor: true, text: true };

    fn lets(self, zone: Zone) -> bool {
        match zone {
            Zone::Always => true,
            Zone::Cursor => self.cursor,
            Zone::Text => self.text,
        }
    }
}

/// When a batch is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Zone {
    Always,
    /// A blinking cursor, drawn in its visible phase.
    Cursor,
    /// Text that asked to blink (SGR 5 or 6) and its decorations.
    Text,
}

/// Instances copied into one GPU buffer, and where each batch lies in it.
struct Packed {
    /// `None` when there is nothing to draw but the clear color.
    buffer: Option<Retained<ProtocolObject<dyn MTLBuffer>>>,
    batches: Vec<Placed>,
    clear: Option<Rgb>,
    /// The glyph atlas generation the glyph instances point into.
    atlas: u64,
}

struct Placed {
    uniforms: Uniforms,
    zone: Zone,
    offset: usize,
    count: usize,
    glyphs: bool,
}

enum Batch<'a> {
    Rects(&'a [RectInstance]),
    Glyphs(&'a [GlyphInstance]),
}

fn pack(device: &Device, batches: &[(Uniforms, Zone, Batch<'_>)], clear: Option<Rgb>, atlas: u64) -> Packed {
    // Buffer offsets must be aligned; 256 suits every Apple GPU.
    const ALIGN: usize = 256;
    let mut bytes = Vec::new();
    let mut placed = Vec::new();
    for (uniforms, zone, batch) in batches {
        let offset = bytes.len().next_multiple_of(ALIGN);
        bytes.resize(offset, 0);
        let (raw, count, glyphs) = match batch {
            Batch::Rects(r) => (as_bytes(r), r.len(), false),
            Batch::Glyphs(g) => (as_bytes(g), g.len(), true),
        };
        bytes.extend_from_slice(raw);
        placed.push(Placed { uniforms: *uniforms, zone: *zone, offset, count, glyphs });
    }
    let buffer = NonNull::new(bytes.as_mut_ptr().cast::<c_void>()).filter(|_| !bytes.is_empty()).and_then(|ptr| {
        // SAFETY: `bytes` is valid for its length, which is copied.
        unsafe { device.newBufferWithBytes_length_options(ptr, bytes.len(), MTLResourceOptions::StorageModeShared) }
    });
    Packed { buffer, batches: placed, clear, atlas }
}

fn as_bytes<T: Copy>(items: &[T]) -> &[u8] {
    // SAFETY: instances are `repr(C)` plain data with explicit padding.
    unsafe { std::slice::from_raw_parts(items.as_ptr().cast::<u8>(), std::mem::size_of_val(items)) }
}

fn pipeline(
    device: &Device,
    library: &ProtocolObject<dyn MTLLibrary>,
    vertex: &str,
    fragment: &str,
) -> Result<Pipeline, RenderError> {
    let function = |name: &str| {
        library.newFunctionWithName(&NSString::from_str(name)).ok_or_else(|| err(&format!("no shader {name}")))
    };
    let descriptor = MTLRenderPipelineDescriptor::new();
    let (vertex, fragment) = (function(vertex)?, function(fragment)?);
    descriptor.setVertexFunction(Some(&vertex));
    descriptor.setFragmentFunction(Some(&fragment));
    // SAFETY: index 0 always exists.
    let color = unsafe { descriptor.colorAttachments().objectAtIndexedSubscript(0) };
    color.setPixelFormat(PIXEL_FORMAT);
    // Premultiplied alpha.
    color.setBlendingEnabled(true);
    color.setSourceRGBBlendFactor(MTLBlendFactor::One);
    color.setSourceAlphaBlendFactor(MTLBlendFactor::One);
    color.setDestinationRGBBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
    color.setDestinationAlphaBlendFactor(MTLBlendFactor::OneMinusSourceAlpha);
    device
        .newRenderPipelineStateWithDescriptor_error(&descriptor)
        .map_err(|e| err(&format!("pipeline: {}", e.localizedDescription())))
}

/// An RGBA glyph atlas texture `size` pixels wide and high.
fn atlas_texture(device: &Device, size: u32) -> Option<Retained<ProtocolObject<dyn MTLTexture>>> {
    // SAFETY: valid format and size.
    let descriptor = unsafe {
        MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
            MTLPixelFormat::RGBA8Unorm,
            size as usize,
            size as usize,
            false,
        )
    };
    descriptor.setUsage(MTLTextureUsage::ShaderRead);
    device.newTextureWithDescriptor(&descriptor)
}

fn err(message: &str) -> RenderError {
    RenderError(message.to_owned())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use objc2_metal::MTLStorageMode;

    use super::*;
    use crate::terminal::{Cell, CellStyle, Cursor, CursorShape, Grapheme, Row};

    /// A renderer drawing into a small texture that tests read back.
    struct Offscreen {
        renderer: Renderer,
        target: Retained<Texture>,
    }

    impl Offscreen {
        const SIZE: usize = 8;

        fn new() -> Self {
            let fonts = Fonts::new(&crate::config::Font::default(), 32.0).expect("fonts");
            let renderer = Renderer::new(fonts).expect("renderer");
            // SAFETY: valid format and size.
            let descriptor = unsafe {
                MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                    PIXEL_FORMAT,
                    Self::SIZE,
                    Self::SIZE,
                    false,
                )
            };
            descriptor.setUsage(MTLTextureUsage::RenderTarget);
            descriptor.setStorageMode(MTLStorageMode::Shared);
            let target = renderer.device.newTextureWithDescriptor(&descriptor).expect("texture");
            Self { renderer, target }
        }

        fn prepare(&mut self, panes: &mut [PaneDraw<'_>]) -> Packed {
            let size = CGSize::new(Self::SIZE as f64, Self::SIZE as f64);
            self.renderer.prepare(size, panes, &[], &[], Some(Rgb { r: 0, g: 0, b: 0 }))
        }

        /// Draws `packed` and reads back its top-left pixel as RGBA.
        fn pixel(&self, packed: &Packed, shown: Shown) -> [u8; 4] {
            let commands = self.renderer.encode(&self.target, packed, shown).expect("drawn");
            commands.commit();
            commands.waitUntilCompleted();
            let mut bgra = [0u8; 4];
            let region =
                MTLRegion { origin: MTLOrigin { x: 0, y: 0, z: 0 }, size: MTLSize { width: 1, height: 1, depth: 1 } };
            let bytes = NonNull::new(bgra.as_mut_ptr().cast::<c_void>()).expect("non-null");
            // SAFETY: one BGRA pixel fits in `bgra`, and the region is inside the texture.
            unsafe { self.target.getBytes_bytesPerRow_fromRegion_mipmapLevel(bytes, 4 * Self::SIZE, region, 0) };
            let [b, g, r, a] = bgra;
            [r, g, b, a]
        }
    }

    /// A blank pane of `background`, two cells wide.
    fn blank_frame(background: Rgb) -> RefCell<Frame> {
        let blank = Cell { text: Grapheme::Empty, fg: background, bg: None, style: CellStyle::default() };
        RefCell::new(Frame {
            cols: 2,
            rows: vec![Row { cells: vec![blank; 2], dirty: true }],
            background,
            foreground: background,
            cursor: None,
            changed: true,
        })
    }

    fn pane<'a>(frame: &'a RefCell<Frame>, tint: Tint) -> PaneDraw<'a> {
        let overlay = Overlay { focused: false, copy_cursor: None, preedit: "" };
        PaneDraw {
            id: "1".parse().expect("pane id"),
            frame: frame.borrow_mut(),
            origin: [0.0, 0.0],
            overlay,
            tint,
            deadline: None,
        }
    }

    fn pixel(background: Rgb, tint: Tint) -> [u8; 4] {
        let mut offscreen = Offscreen::new();
        let frame = blank_frame(background);
        let packed = offscreen.prepare(&mut [pane(&frame, tint)]);
        offscreen.pixel(&packed, Shown::ALL)
    }

    #[test]
    fn a_blinking_cursor_is_left_out_in_its_hidden_phase() {
        let mut offscreen = Offscreen::new();
        let (black, red) = (Rgb { r: 0, g: 0, b: 0 }, Rgb { r: 255, g: 0, b: 0 });
        let frame = blank_frame(black);
        let cursor = Cursor { x: 0, y: 0, shape: CursorShape::Block, wide: false, color: Some(red), blinking: true };
        frame.borrow_mut().cursor = Some(cursor);
        let mut pane = pane(&frame, Tint::NONE);
        pane.overlay.focused = true;
        let packed = offscreen.prepare(&mut [pane]);
        assert_eq!(offscreen.pixel(&packed, Shown::ALL), [255, 0, 0, 255]);
        assert_eq!(offscreen.pixel(&packed, Shown { cursor: false, text: true }), [0, 0, 0, 255]);
    }

    #[test]
    fn a_surface_knows_when_its_text_blinks() {
        let mut offscreen = Offscreen::new();
        let frame = blank_frame(Rgb { r: 0, g: 0, b: 0 });
        let mut surface = Surface::new(&offscreen.renderer.device, 1.0);
        surface.packed = Some(offscreen.prepare(&mut [pane(&frame, Tint::NONE)]));
        assert!(!surface.blinks_text());
        {
            let mut frame = frame.borrow_mut();
            let cell = &mut frame.rows[0].cells[0];
            cell.text = Grapheme::Char('x');
            cell.style.blink = true;
            frame.rows[0].dirty = true;
        }
        surface.packed = Some(offscreen.prepare(&mut [pane(&frame, Tint::NONE)]));
        assert!(surface.blinks_text());
    }

    #[test]
    fn an_untinted_pane_keeps_its_background() {
        assert_eq!(pixel(Rgb { r: 200, g: 40, b: 40 }, Tint::NONE), [200, 40, 40, 255]);
    }

    #[test]
    fn a_tint_scales_saturation_and_brightness() {
        // Saturation 0.5 moves each channel halfway to the largest; brightness
        // 0.5 then halves all three: (200, 120, 120) -> (100, 60, 60).
        let [r, g, b, _] = pixel(Rgb { r: 200, g: 40, b: 40 }, Tint { saturation: 0.5, brightness: 0.5 });
        let near = |got: u8, want: u8| got.abs_diff(want) <= 1;
        assert!(near(r, 100) && near(g, 60) && near(b, 60), "got {r}, {g}, {b}");
    }
}
