// The video and the interface, put together on the GPU (see render.rs). Compiled at run time with
// newLibraryWithSource, so this file is plain MSL with no includes of our own.
//
// The video pass does what the D3D11 video processor and Direct2D's effects did on Windows: YCbCr
// to RGB with the frame's matrix and range, the colour adjustments, and for BT.2020 the tone map
// of tonemap.rs. Geometry (crop, aspect, zoom, rotation, mirroring) is all in the quad's corners.

#include <metal_stdlib>
using namespace metal;

/// One corner of a quad: clip-space position and where it reads the source, in texture uv.
struct Corner {
    float2 position;
    float2 uv;
};

struct Varyings {
    float4 position [[position]];
    float2 uv;
};

/// Four corners drawn as a triangle strip: top-left, top-right, bottom-left, bottom-right.
vertex Varyings corners(uint id [[vertex_id]], constant Corner *corners [[buffer(0)]]) {
    Varyings out;
    out.position = float4(corners[id].position, 0.0, 1.0);
    out.uv = corners[id].uv;
    return out;
}

/// Mirrors render.rs's `Params`; only float4 and int4 members, so both sides lay it out alike.
struct Params {
    /// Samples to Y (0 black, 1 white) and Cb, Cr (-0.5..0.5): sample * scale - offset.
    float4 scale;
    float4 offset;
    /// xy: where the chroma plane is read, relative to the luma position, in its uv.
    float4 chroma;
    /// Rows of the YCbCr to R'G'B' matrix.
    float4 matrix[3];
    /// x contrast gain, y brightness offset, z saturation gain.
    float4 adjust;
    /// x cos, y sin of the hue rotation.
    float4 hue;
    /// Rows of the matrix from decoded light to the output's primaries.
    float4 gamut[3];
    /// x nits of decoded light 1.0, y the stream's peak, z the light the curve's 1.0 stands
    /// for, w the output value of the curve's 1.0.
    float4 tone;
    /// x the signal: 0 BT.709 or BT.601, 1 PQ, 2 HLG, 3 SDR BT.2020. y the output: 0 sRGB, 1
    /// linear light for an EDR layer (1.0 is SDR white).
    int4 mode;
};

constant float KNEE = 0.75;
constant float3 LUMA_2020 = float3(0.2627, 0.6780, 0.0593);

float3 apply(constant float4 *rows, float3 v) {
    return float3(dot(rows[0].xyz, v), dot(rows[1].xyz, v), dot(rows[2].xyz, v));
}

/// SMPTE ST 2084: light, 1.0 for 10000 nits, of signal `e`.
float3 pq_light(float3 e) {
    const float m1 = 0.1593017578125, m2 = 78.84375;
    const float c1 = 0.8359375, c2 = 18.8515625, c3 = 18.6875;
    float3 p = pow(e, 1.0 / m2);
    return pow(max(p - c1, 0.0) / (c2 - c3 * p), 1.0 / m1);
}

/// HLG's inverse OETF (BT.2100): scene light, 0..1, of signal `e`.
float3 hlg_scene(float3 e) {
    const float a = 0.17883277, b = 0.28466892, c = 0.55991073;
    return select((exp((e - c) / a) + b) / 12.0, e * e / 3.0, e <= 0.5);
}

/// The sRGB curve, decoding and encoding.
float3 srgb_linear(float3 v) {
    return select(pow((v + 0.055) / 1.055, 2.4), v / 12.92, v <= 0.04045);
}

float3 srgb_encode(float3 l) {
    return select(1.055 * pow(l, 1.0 / 2.4) - 0.055, l * 12.92, l <= 0.0031308);
}

/// tonemap.rs's `curve` per channel: light at `x` times the stream's peak, as a level where
/// 1.0 is `top`. Unchanged up to the knee, then extended Reinhard up to the peak.
float3 curve(float3 x, float peak, float top) {
    float3 level = x * peak / top;
    float max_level = peak / top;
    if (max_level <= 1.0) {
        return min(level, 1.0);
    }
    float3 u = (level - KNEE) / (1.0 - KNEE);
    float w = (max_level - KNEE) / (1.0 - KNEE);
    float3 rolled = u * (1.0 + u / (w * w)) / (1.0 + u);
    return min(select(KNEE + (1.0 - KNEE) * rolled, level, level <= KNEE), 1.0);
}

fragment float4 video(Varyings in [[stage_in]],
                      texture2d<float> luma [[texture(0)]],
                      texture2d<float> chroma [[texture(1)]],
                      sampler linear [[sampler(0)]],
                      constant Params &p [[buffer(0)]]) {
    float y = luma.sample(linear, in.uv).r;
    float2 c = chroma.sample(linear, in.uv + p.chroma.xy).rg;
    float3 ycc = float3(y, c) * p.scale.xyz - p.offset.xyz;

    // VLC's adjust filter: contrast and brightness on luma, hue and saturation on chroma.
    ycc.x = ycc.x * p.adjust.x + p.adjust.y;
    ycc.yz = float2(ycc.y * p.hue.x + ycc.z * p.hue.y, ycc.z * p.hue.x - ycc.y * p.hue.y)
             * p.adjust.z;
    float3 rgb = clamp(apply(p.matrix, ycc), 0.0, 1.0);

    int signal = p.mode.x;
    bool edr = p.mode.y == 1;
    if (signal == 0) {
        // BT.709 and BT.601 show as their R'G'B' values are, as sRGB, the way every player does.
        return float4(edr ? apply(p.gamut, srgb_linear(rgb)) : rgb, 1.0);
    }

    // BT.2020: to light in the output's primaries, `tone.x` nits per 1.0.
    float3 light;
    if (signal == 1) {
        light = pq_light(rgb);
    } else if (signal == 2) {
        // The system gamma (1.2 for a 1000-nit display) goes by luminance, so colours keep
        // their saturation.
        float3 scene = hlg_scene(rgb);
        light = scene * pow(clamp(dot(scene, LUMA_2020), 0.0, 1.0), 0.2);
    } else {
        // BT.1886 both ways round the matrix, so SDR BT.2020 keeps the tones BT.709 video has.
        light = pow(rgb, 2.4);
    }
    light = apply(p.gamut, light);
    if (signal == 3) {
        light = srgb_linear(pow(clamp(light, 0.0, 1.0), 1.0 / 2.4));
    }
    float3 level = curve(clamp(light * p.tone.x / p.tone.y, 0.0, 1.0), p.tone.y, p.tone.z);
    return float4(edr ? level * p.tone.w : srgb_encode(level), 1.0);
}

struct Interface {
    /// Rows of linear sRGB to the output's primaries, for an EDR layer.
    float4 gamut[3];
    /// x: 0 for an sRGB output, 1 for linear light.
    int4 mode;
};

/// The interface's premultiplied BGRA bitmap, pixel for pixel, blended over the video.
fragment float4 interface(Varyings in [[stage_in]],
                          texture2d<float> ui [[texture(0)]],
                          constant Interface &p [[buffer(0)]]) {
    float4 c = ui.read(uint2(in.position.xy));
    if (p.mode.x == 0 || c.a <= 0.0) {
        return c;
    }
    float3 light = apply(p.gamut, srgb_linear(c.rgb / c.a));
    return float4(light * c.a, c.a);
}
