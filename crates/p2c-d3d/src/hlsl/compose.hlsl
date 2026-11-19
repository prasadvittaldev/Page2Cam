// Compositing: the page letterboxed into the output, then the webcam PIP
// (Box: opaque; cut-out: webcam x remapped mask, premultiplied, blended
// ONE / INV_SRC_ALPHA over the page by the output-merger).
//
// The PIP is built into a layer texture at the PIP rectangle's size
// (ps_compose) only when the webcam, mask or size changes, and drawn 1:1
// every tick (ps_layer).
//
// Mirrors p2c_compositor::Compositor: downscaling uses the same separable
// triangle ("bilinear convolution") filter as fast_image_resize, which
// widens the kernel when downscaling; values are rounded to 0..255 where the
// CPU stores bytes (scaled webcam, scaled mask, edge remap, premultiply).
// Upscales (where that filter is plain bilinear interpolation) may use the
// hardware bilinear sampler instead (cam_linear / mask_linear).
//
// All passes draw the fullscreen triangle of nv12.hlsl into a viewport set
// to the destination rectangle (x may be negative: the cut-out overhang).

Texture2D<float4> src : register(t0);   // page, webcam or PIP layer (BGRA)
Texture2D<float> mask : register(t1);   // person mask (R8)
SamplerState lin : register(s0);        // bilinear, clamp

cbuffer PassConsts : register(b0) {
    float4 rect;      // destination rectangle in output pixels: x, y, w, h
    float2 src_size;  // source texture size in pixels
    float2 mask_size; // mask texture size in pixels
    uint mode;        // 0 = opaque (page, Box), 1 = cut-out
    float edge_lo;    // EdgeRange.lo
    float edge_hi;    // EdgeRange.hi
    uint edge_full;   // 1 = leave the mask as it is (EdgeRange::FULL or hi <= lo)
    uint cam_linear;  // 1 = scale src with the hardware bilinear sampler
    uint mask_linear; // 1 = scale the mask with the hardware bilinear sampler
    float2 pad;
};

// Weight of source pixel `i` for a destination pixel whose centre maps to
// `c` in source pixels, kernel scale `fs` (>= 1).
float tri(float i, float c, float fs) {
    return max(0.0, 1.0 - abs((i + 0.5 - c) / fs));
}

// fast_image_resize rounds like this ((sum + half) >> precision): half up.
float4 round_up4(float4 v) { return floor(v + 0.5); }
float round_up(float v) { return floor(v + 0.5); }

// Source value (0..255 units, rounded to an integer) at destination-local
// pixel centre `p` (pixels from the rectangle's top-left), filtered like
// fast_image_resize's bilinear convolution for u8 images: a vertical pass
// rounded to bytes, then a horizontal pass (weights normalised over the
// in-range pixels, i.e. clamp at the edges).
float4 resample_rgba(float2 p, float2 ssize, float2 dsize) {
    float2 scale = ssize / dsize;
    if (all(scale == 1.0)) {
        return round_up4(src.Load(int3(int2(p), 0)) * 255.0);
    }
    float2 fs = max(scale, 1.0);
    float2 c = p * scale;
    int2 lo = max(int2(floor(c - fs)), 0);
    int2 hi = min(int2(ceil(c + fs)), int2(ssize));
    float wy_sum = 0.0;
    for (int y = lo.y; y < hi.y; y++) {
        wy_sum += tri(y, c.y, fs.y);
    }
    float4 acc = 0.0;
    float wx_sum = 0.0;
    [loop] for (int x = lo.x; x < hi.x; x++) {
        float wx = tri(x, c.x, fs.x);
        if (wx > 0.0) {
            float4 col = 0.0;
            [loop] for (int y = lo.y; y < hi.y; y++) {
                col += tri(y, c.y, fs.y) * round_up4(src.Load(int3(x, y, 0)) * 255.0);
            }
            acc += wx * round_up4(col / wy_sum);
            wx_sum += wx;
        }
    }
    return clamp(round_up4(acc / wx_sum), 0.0, 255.0);
}

float resample_mask(float2 p, float2 ssize, float2 dsize) {
    float2 scale = ssize / dsize;
    if (all(scale == 1.0)) {
        return round_up(mask.Load(int3(int2(p), 0)) * 255.0);
    }
    float2 fs = max(scale, 1.0);
    float2 c = p * scale;
    int2 lo = max(int2(floor(c - fs)), 0);
    int2 hi = min(int2(ceil(c + fs)), int2(ssize));
    float wy_sum = 0.0;
    for (int y = lo.y; y < hi.y; y++) {
        wy_sum += tri(y, c.y, fs.y);
    }
    float acc = 0.0;
    float wx_sum = 0.0;
    [loop] for (int x = lo.x; x < hi.x; x++) {
        float wx = tri(x, c.x, fs.x);
        if (wx > 0.0) {
            float col = 0.0;
            [loop] for (int y = lo.y; y < hi.y; y++) {
                col += tri(y, c.y, fs.y) * round_up(mask.Load(int3(x, y, 0)) * 255.0);
            }
            acc += wx * round_up(col / wy_sum);
            wx_sum += wx;
        }
    }
    return clamp(round_up(acc / wx_sum), 0.0, 255.0);
}

// Same as EdgeRange::lut: smoothstep between lo and hi, as a 0..255 integer.
float edge_remap(float m) {
    if (edge_full != 0) {
        return m;
    }
    float x = saturate((m - edge_lo) / (edge_hi - edge_lo));
    return round_up(x * x * (3.0 - 2.0 * x) * 255.0);
}

struct VsOut {
    float4 pos : SV_Position;
};

float4 ps_compose(VsOut i) : SV_Target {
    float2 p = i.pos.xy - rect.xy; // destination-local pixel centre
    // Bilinear sampling at p / dsize puts the destination centre at
    // p * scale source pixels, like the convolution.
    // (if/else, not ?: — HLSL evaluates both sides of ?:.)
    float4 c;
    [branch] if (cam_linear != 0) {
        c = round_up4(src.SampleLevel(lin, p / rect.zw, 0) * 255.0);
    } else {
        c = resample_rgba(p, src_size, rect.zw);
    }
    if (mode == 0) {
        return float4(c.rgb / 255.0, 1.0);
    }
    float m;
    [branch] if (mask_linear != 0) {
        m = round_up(mask.SampleLevel(lin, p / rect.zw, 0) * 255.0);
    } else {
        m = resample_mask(p, mask_size, rect.zw);
    }
    float a = edge_remap(m);
    // Premultiply like the CPU: (c * a + 127) / 255 in integers.
    uint3 pm = (uint3(c.rgb) * uint(a) + 127) / 255;
    return float4(float3(pm) / 255.0, a / 255.0);
}

// The cached PIP layer (premultiplied, at the rectangle's size), 1:1.
float4 ps_layer(VsOut i) : SV_Target {
    return src.Load(int3(int2(i.pos.xy - rect.xy), 0));
}

// Plain bilinear scaling (the admin preview at a downscale of at most 2).
float4 ps_sample(VsOut i) : SV_Target {
    return src.SampleLevel(lin, (i.pos.xy - rect.xy) / rect.zw, 0);
}
