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

use super::atlas::{ATLAS_SIZE, Atlas};
use super::scene::{GlyphInstance, Line, Overlay, RectInstance, Scene};
use crate::font::{Fonts, Metrics};
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
struct Uniforms {
    viewport: [f32; 2],
    origin: [f32; 2],
}

type Device = ProtocolObject<dyn MTLDevice>;
type Pipeline = Retained<ProtocolObject<dyn MTLRenderPipelineState>>;

pub struct Renderer {
    device: Retained<Device>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    rect_pipeline: Pipeline,
    glyph_pipeline: Pipeline,
    texture: Retained<ProtocolObject<dyn MTLTexture>>,
    fonts: Fonts,
    atlas: Atlas,
    scene: Scene,
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
            scene: Scene::default(),
        })
    }

    pub fn device(&self) -> &Device {
        &self.device
    }

    pub fn metrics(&self) -> Metrics {
        self.fonts.metrics()
    }

    /// Swaps the fonts, after a change of display scale.
    pub fn set_fonts(&mut self, fonts: Fonts) {
        self.fonts = fonts;
        self.atlas.reset();
        self.scene.invalidate();
    }

    /// Draws `frame` into `layer`, with the grid's top-left corner at
    /// `origin` (device pixels), and the `chrome` lines over it.
    pub fn render(
        &mut self,
        layer: &CAMetalLayer,
        frame: &mut Frame,
        overlay: &Overlay<'_>,
        origin: [f32; 2],
        chrome: &[Line],
    ) {
        self.scene.build(frame, overlay, chrome, &self.fonts, &mut self.atlas);
        self.upload_glyphs();

        let Some(drawable) = layer.nextDrawable() else {
            log::warn!("no drawable; skipping a frame");
            return;
        };
        let size = layer.drawableSize();
        let viewport = [size.width as f32, size.height as f32];
        let grid = Uniforms { viewport, origin };
        let screen = Uniforms { viewport, origin: [0.0, 0.0] };
        if let Err(e) = self.encode(&drawable, [&grid, &screen], frame.background) {
            log::error!("drawing failed: {e}");
        }
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

    /// Draws the scene: the grid batches with the first uniforms, the chrome
    /// with the second.
    fn encode(
        &self,
        drawable: &ProtocolObject<dyn CAMetalDrawable>,
        [grid, screen]: [&Uniforms; 2],
        clear: Rgb,
    ) -> Result<(), RenderError> {
        let pass = MTLRenderPassDescriptor::new();
        // SAFETY: index 0 always exists.
        let color = unsafe { pass.colorAttachments().objectAtIndexedSubscript(0) };
        color.setTexture(Some(&drawable.texture()));
        color.setLoadAction(MTLLoadAction::Clear);
        color.setStoreAction(MTLStoreAction::Store);
        let c = |v: u8| f64::from(v) / 255.0;
        color.setClearColor(MTLClearColor { red: c(clear.r), green: c(clear.g), blue: c(clear.b), alpha: 1.0 });

        let commands = self.queue.commandBuffer().ok_or_else(|| err("no command buffer"))?;
        let encoder = commands.renderCommandEncoderWithDescriptor(&pass).ok_or_else(|| err("no encoder"))?;
        let s = &self.scene;
        let batches = [
            (grid, Batch::Rects(&s.rects)),
            (grid, Batch::Glyphs(&s.glyphs)),
            (grid, Batch::Rects(&s.overlay_rects)),
            (grid, Batch::Glyphs(&s.overlay_glyphs)),
            (screen, Batch::Rects(&s.chrome_rects)),
            (screen, Batch::Glyphs(&s.chrome_glyphs)),
        ];
        if let Some((buffer, layout)) = pack(&self.device, batches.iter().map(|(_, batch)| batch)) {
            // SAFETY: the uniforms outlive the calls, which copy them; every
            // offset and count describes instances inside `buffer`.
            unsafe {
                encoder.setFragmentTexture_atIndex(Some(&self.texture), 0);
                for ((uniforms, _), (offset, count, glyphs)) in batches.iter().zip(layout) {
                    if count == 0 {
                        continue;
                    }
                    let uniforms: &Uniforms = uniforms;
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
        commands.presentDrawable(ProtocolObject::<dyn MTLDrawable>::from_ref(drawable));
        commands.commit();
        Ok(())
    }
}

enum Batch<'a> {
    Rects(&'a [RectInstance]),
    Glyphs(&'a [GlyphInstance]),
}

/// Copies the batches into one buffer. Returns it with each batch's offset,
/// instance count and kind (glyphs or rects).
#[allow(clippy::type_complexity, reason = "a private tuple read right away")]
fn pack<'a>(
    device: &Device,
    batches: impl Iterator<Item = &'a Batch<'a>>,
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
