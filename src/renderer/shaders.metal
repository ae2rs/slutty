#include <metal_stdlib>
using namespace metal;

// Layouts match RectInstance and GlyphInstance in scene.rs.
struct Uniforms {
    float2 viewport;
    float2 origin;
};

struct RectInstance {
    float2 pos;
    float2 size;
    uchar4 color;
    uint pad;
};

struct GlyphInstance {
    float2 pos;
    float2 size;
    float2 uv;
    uchar4 color;
    uint colored;
};

struct RectOut {
    float4 position [[position]];
    half4 color;
};

struct GlyphOut {
    float4 position [[position]];
    float2 uv;
    half4 color;
    uint colored [[flat]];
};

static float4 clip(float2 px, constant Uniforms& u) {
    float2 p = (px + u.origin) / u.viewport * 2.0 - 1.0;
    return float4(p.x, -p.y, 0.0, 1.0);
}

// Triangle strip corners: (0,0) (1,0) (0,1) (1,1).
static float2 corner(uint vid) {
    return float2(vid & 1, vid >> 1);
}

vertex RectOut rect_vertex(
    uint vid [[vertex_id]],
    uint iid [[instance_id]],
    constant Uniforms& u [[buffer(0)]],
    const device RectInstance* rects [[buffer(1)]]
) {
    RectInstance r = rects[iid];
    RectOut out;
    out.position = clip(r.pos + corner(vid) * r.size, u);
    half4 c = half4(r.color) / 255.0h;
    out.color = half4(c.rgb * c.a, c.a);
    return out;
}

fragment half4 rect_fragment(RectOut in [[stage_in]]) {
    return in.color;
}

vertex GlyphOut glyph_vertex(
    uint vid [[vertex_id]],
    uint iid [[instance_id]],
    constant Uniforms& u [[buffer(0)]],
    const device GlyphInstance* glyphs [[buffer(1)]]
) {
    GlyphInstance g = glyphs[iid];
    float2 c = corner(vid);
    GlyphOut out;
    out.position = clip(g.pos + c * g.size, u);
    out.uv = g.uv + c * g.size;
    out.color = half4(g.color) / 255.0h;
    out.colored = g.colored;
    return out;
}

fragment half4 glyph_fragment(GlyphOut in [[stage_in]], texture2d<half> atlas [[texture(0)]]) {
    constexpr sampler s(coord::pixel, filter::nearest);
    // The atlas is premultiplied; text is white, so alpha is its coverage.
    half4 t = atlas.sample(s, in.uv);
    if (in.colored != 0) {
        return t;
    }
    return half4(in.color.rgb, 1.0h) * (t.a * in.color.a);
}
