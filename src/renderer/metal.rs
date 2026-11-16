use std::cell::RefMut;
use std::collections::HashMap;
use std::ffi::c_void;
use std::ptr::NonNull;

use libghostty_vt::color::Rgb;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2_foundation::NSString;
use objc2_metal::{
    MTLBlendFactor, MTLBuffer, MTLClearColor, MTLCommandBuffer, MTLCommandEncoder, MTLCommandQueue,
    MTLCreateSystemDefaultDevice, MTLDevice, MTLDrawable, MTLLibrary, MTLLoadAction, MTLOrigin, MTLPixelFormat,
    MTLPrimitiveType, MTLRegion, MTLRenderCommandEncoder, MTLRenderPassDescriptor, MTLRenderPipelineDescriptor,
    MTLRenderPipelineState, MTLResourceOptions, MTLSize, MTLStoreAction, MTLTexture, MTLTextureDescriptor,
    MTLTextureUsage,
};
use objc2_quartz_core::{CAMetalDrawable, CAMetalLayer};

use super::atlas::{ATLAS_SIZE, Atlas, Full};
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
}

pub struct Renderer {
    device: Retained<Device>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    rect_pipeline: Pipeline,
    glyph_pipeline: Pipeline,
    texture: Retained<ProtocolObject<dyn MTLTexture>>,
    fonts: Fonts,
    atlas: Atlas,
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

        // SAFETY: valid format and size.
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::RGBA8Unorm,
                ATLAS_SIZE as usize,
                ATLAS_SIZE as usize,
                false,
            )
        };
        descriptor.setUsage(MTLTextureUsage::ShaderRead);
        let texture = device.newTextureWithDescriptor(&descriptor).ok_or_else(|| err("no atlas texture"))?;

        Ok(Self {
            device,
            queue,
            rect_pipeline,
            glyph_pipeline,
            texture,
            fonts,
            atlas: Atlas::default(),
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

    /// Draws the panes into `layer` over `clear`, then `rects` (device
    /// pixels from the top-left), then the `chrome` lines.
    pub fn render(
        &mut self,
        layer: &CAMetalLayer,
        panes: &mut [PaneDraw<'_>],
        rects: &[RectInstance],
        chrome: &[Line],
        clear: Rgb,
    ) {
        let Some(drawable) = layer.nextDrawable() else {
            log::warn!("no drawable; skipping a frame");
            return;
        };
        match self.draw(&drawable.texture(), panes, rects, chrome, clear) {
            Ok(commands) => {
                commands.presentDrawable(ProtocolObject::<dyn MTLDrawable>::from_ref(&*drawable));
                commands.commit();
            }
            Err(e) => log::error!("drawing failed: {e}"),
        }
    }

    /// Encodes the panes, `rects` and `chrome` into `target`. The caller
    /// commits the commands.
    fn draw(
        &mut self,
        target: &Texture,
        panes: &mut [PaneDraw<'_>],
        rects: &[RectInstance],
        chrome: &[Line],
        clear: Rgb,
    ) -> Result<Commands, RenderError> {
        self.scenes.retain(|id, _| panes.iter().any(|p| p.id == *id));
        if self.build(panes, chrome).is_err() {
            log::info!("glyph atlas full; starting over");
            self.atlas.reset();
            self.scenes.values_mut().for_each(Scene::invalidate);
            if self.build(panes, chrome).is_err() {
                log::warn!("glyph atlas too small for one screen");
            }
        }
        self.upload_glyphs();

        let viewport = [target.width() as f32, target.height() as f32];
        let mut batches = Vec::new();
        for pane in panes.iter() {
            let Some(s) = self.scenes.get(&pane.id) else { continue };
            let uniforms = Uniforms { viewport, origin: pane.origin, tint: pane.tint };
            batches.extend([
                (uniforms, Batch::Rects(&s.rects)),
                (uniforms, Batch::Glyphs(&s.glyphs)),
                (uniforms, Batch::Rects(&s.overlay_rects)),
                (uniforms, Batch::Glyphs(&s.overlay_glyphs)),
            ]);
        }
        let screen = Uniforms { viewport, origin: [0.0, 0.0], tint: Tint::NONE };
        batches.extend([
            (screen, Batch::Rects(rects)),
            (screen, Batch::Rects(&self.chrome.rects)),
            (screen, Batch::Glyphs(&self.chrome.glyphs)),
        ]);
        self.encode(target, &batches, clear)
    }

    fn build(&mut self, panes: &mut [PaneDraw<'_>], chrome: &[Line]) -> Result<(), Full> {
        for pane in panes {
            let scene = self.scenes.entry(pane.id).or_default();
            scene.build(&mut pane.frame, &pane.overlay, &self.fonts, &mut self.atlas)?;
        }
        self.chrome.build(chrome, &self.fonts, &mut self.atlas)
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

    /// Draws each batch with its uniforms, in order.
    fn encode(&self, target: &Texture, batches: &[(Uniforms, Batch<'_>)], clear: Rgb) -> Result<Commands, RenderError> {
        let pass = MTLRenderPassDescriptor::new();
        // SAFETY: index 0 always exists.
        let color = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        color.setTexture(Some(target));
        color.setLoadAction(MTLLoadAction::Clear);
        color.setStoreAction(MTLStoreAction::Store);
        let c = |v: u8| f64::from(v) / 255.0;
        color.setClearColor(MTLClearColor { red: c(clear.r), green: c(clear.g), blue: c(clear.b), alpha: 1.0 });

        let commands = self.queue.commandBuffer().ok_or_else(|| err("no command buffer"))?;
        let encoder = commands.renderCommandEncoderWithDescriptor(&pass).ok_or_else(|| err("no encoder"))?;
        if let Some((buffer, layout)) = pack(&self.device, batches.iter().map(|(_, batch)| batch)) {
            // SAFETY: the uniforms outlive the calls, which copy them; every
            // offset and count describes instances inside `buffer`.
            unsafe {
                encoder.setFragmentTexture_atIndex(Some(&self.texture), 0);
                for ((uniforms, _), (offset, count, glyphs)) in batches.iter().zip(layout) {
                    if count == 0 {
                        continue;
                    }
                    encoder.setVertexBytes_length_atIndex(NonNull::from(uniforms).cast(), size_of::<Uniforms>(), 0);
                    encoder.setRenderPipelineState(if glyphs { &self.glyph_pipeline } else { &self.rect_pipeline });
                    encoder.setVertexBuffer_offset_atIndex(Some(&buffer), offset, 1);
                    encoder.drawPrimitives_vertexStart_vertexCount_instanceCount(
                        MTLPrimitiveType::TriangleStrip,
                        0,
                        4,
                        count,
                    );
                }
            }
        }
        encoder.endEncoding();
        Ok(commands)
    }
}

enum Batch<'a> {
    Rects(&'a [RectInstance]),
    Glyphs(&'a [GlyphInstance]),
}

/// Copies the batches into one buffer. Returns it with each batch's offset,
/// instance count and kind (glyphs or rects).
#[allow(clippy::type_complexity, reason = "a private tuple read right away")]
fn pack<'a, 'b: 'a>(
    device: &Device,
    batches: impl Iterator<Item = &'a Batch<'b>>,
) -> Option<(Retained<ProtocolObject<dyn MTLBuffer>>, Vec<(usize, usize, bool)>)> {
    // Buffer offsets must be aligned; 256 suits every Apple GPU.
    const ALIGN: usize = 256;
    let mut bytes = Vec::new();
    let mut layout = Vec::new();
    for batch in batches {
        let offset = bytes.len().next_multiple_of(ALIGN);
        bytes.resize(offset, 0);
        let (raw, count, glyphs) = match batch {
            Batch::Rects(r) => (as_bytes(r), r.len(), false),
            Batch::Glyphs(g) => (as_bytes(g), g.len(), true),
        };
        bytes.extend_from_slice(raw);
        layout.push((offset, count, glyphs));
    }
    let ptr = NonNull::new(bytes.as_mut_ptr().cast::<c_void>())?;
    // SAFETY: `bytes` is valid for its length, which is copied.
    let buffer =
        unsafe { device.newBufferWithBytes_length_options(ptr, bytes.len(), MTLResourceOptions::StorageModeShared) }?;
    Some((buffer, layout))
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

fn err(message: &str) -> RenderError {
    RenderError(message.to_owned())
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use objc2_metal::MTLStorageMode;

    use super::*;
    use crate::terminal::{Cell, CellStyle, Grapheme, Row};

    /// Draws one blank pane of `background` under `tint` off screen, and
    /// reads back its top-left pixel as RGBA.
    fn pixel(background: Rgb, tint: Tint) -> [u8; 4] {
        let fonts = Fonts::new(&crate::config::Font::default(), 32.0).expect("fonts");
        let mut renderer = Renderer::new(fonts).expect("renderer");
        let (width, height) = (8, 8);
        // SAFETY: valid format and size.
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                PIXEL_FORMAT,
                width,
                height,
                false,
            )
        };
        descriptor.setUsage(MTLTextureUsage::RenderTarget);
        descriptor.setStorageMode(MTLStorageMode::Shared);
        let target = renderer.device.newTextureWithDescriptor(&descriptor).expect("texture");

        let blank = Cell { text: Grapheme::Empty, fg: background, bg: None, style: CellStyle::default() };
        let frame = RefCell::new(Frame {
            cols: 2,
            rows: vec![Row { cells: vec![blank; 2], dirty: true }],
            background,
            foreground: background,
            cursor: None,
            changed: true,
        });
        let overlay = Overlay { focused: false, copy_cursor: None, preedit: "" };
        let id = "1".parse().expect("pane id");
        let mut panes = [PaneDraw { id, frame: frame.borrow_mut(), origin: [0.0, 0.0], overlay, tint }];
        let black = Rgb { r: 0, g: 0, b: 0 };
        let commands = renderer.draw(&target, &mut panes, &[], &[], black).expect("drawn");
        commands.commit();
        commands.waitUntilCompleted();

        let mut bgra = [0u8; 4];
        let region =
            MTLRegion { origin: MTLOrigin { x: 0, y: 0, z: 0 }, size: MTLSize { width: 1, height: 1, depth: 1 } };
        let bytes = NonNull::new(bgra.as_mut_ptr().cast::<c_void>()).expect("non-null");
        // SAFETY: one BGRA pixel fits in `bgra`, and the region is inside the texture.
        unsafe { target.getBytes_bytesPerRow_fromRegion_mipmapLevel(bytes, 4 * width, region, 0) };
        let [b, g, r, a] = bgra;
        [r, g, b, a]
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
