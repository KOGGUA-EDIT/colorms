//! A small CPU triangle rasterizer for egui's tessellated output.
//!
//! egui turns the whole interface into triangles; this module rasterizes them
//! into BGRA pixels that GDI blits to the window. Doing the work on the CPU
//! means Colorms never creates an OpenGL or Direct3D device — and GPU driver
//! initialisation (`ChoosePixelFormat` costs ~180 ms) was the dominant cost of
//! the old launch.
//!
//! The rasterizer is span based: every triangle is cut into horizontal spans
//! whose ends are found by interpolating the three edges, and every span picks
//! one of four specialised kernels (opaque fill, constant-alpha blend, vertex
//! colour gradient, textured bilinear). No division happens inside a span and
//! no barycentric walk is needed, which keeps egui's tens of thousands of tiny
//! anti-aliasing triangles cheap.

use std::collections::HashMap;
use std::sync::Arc;

use egui::epaint::{Mesh, Primitive, Vertex};
use egui::{ClippedPrimitive, Color32, TextureId, TexturesDelta};

/// Premultiplied sRGB texel packed as `0xAARRGGBB`.
type Texel = u32;

#[derive(Clone)]
pub struct Texture {
    width: usize,
    height: usize,
    linear: bool,
    pixels: Vec<Texel>,
}

impl Texture {
    fn from_image(image: &egui::ColorImage, options: egui::TextureOptions) -> Self {
        let mut pixels = Vec::with_capacity(image.pixels.len());
        for pixel in &image.pixels {
            let [r, g, b, a] = pixel.to_array();
            pixels.push(pack(r, g, b, a));
        }
        Self {
            width: image.width().max(1),
            height: image.height().max(1),
            linear: matches!(options.magnification, egui::TextureFilter::Linear),
            pixels,
        }
    }

    fn update(&mut self, image: &egui::ColorImage, origin: [usize; 2]) {
        let (ox, oy) = (origin[0], origin[1]);
        for y in 0..image.height() {
            let ty = oy + y;
            if ty >= self.height {
                break;
            }
            for x in 0..image.width() {
                let tx = ox + x;
                if tx >= self.width {
                    break;
                }
                let [r, g, b, a] = image.pixels[y * image.width() + x].to_array();
                self.pixels[ty * self.width + tx] = pack(r, g, b, a);
            }
        }
    }
}

#[inline(always)]
fn pack(r: u8, g: u8, b: u8, a: u8) -> Texel {
    ((a as u32) << 24) | ((r as u32) << 16) | ((g as u32) << 8) | b as u32
}

/// `(a * b) / 255` for 8-bit operands, exact over the whole range.
#[inline(always)]
fn mul255(a: u32, b: u32) -> u32 {
    let t = a * b + 128;
    (t + (t >> 8)) >> 8
}

/// Source-over blending for premultiplied 8-bit colours.
#[inline(always)]
fn over(dst: u32, r: u32, g: u32, b: u32, inv: u32) -> u32 {
    let dr = r + mul255((dst >> 16) & 0xFF, inv);
    let dg = g + mul255((dst >> 8) & 0xFF, inv);
    let db = b + mul255(dst & 0xFF, inv);
    0xFF00_0000 | (dr.min(255) << 16) | (dg.min(255) << 8) | db.min(255)
}

#[derive(Clone, Copy, Default)]
struct Clip {
    x0: i32,
    y0: i32,
    x1: i32,
    y1: i32,
}

impl Clip {
    #[inline(always)]
    fn is_empty(self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
}

/// How a single triangle is painted.
enum Shade<'a> {
    /// One colour for the whole triangle.
    Solid { r: u32, g: u32, b: u32, a: u32 },
    /// Vertex colours blended across the triangle (gradient meshes).
    Gradient([f32; 4], [f32; 4], [f32; 4]),
    /// A texture, optionally tinted by a constant colour.
    Textured {
        texture: &'a Texture,
        u0: [f32; 2],
        u1: [f32; 2],
        u2: [f32; 2],
        tint: Option<[u32; 4]>,
    },
}

/// The offscreen surface every frame is painted into.
pub struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<Texel>,
    textures: HashMap<TextureId, Arc<Texture>>,
    filled: u64,
    spans: u64,
    px_solid_fill: u64,
    px_solid_over: u64,
    px_grad_row: u64,
    px_grad: u64,
    px_tex_linear: u64,
    px_tex_nearest: u64,
}

impl Canvas {
    pub fn new() -> Self {
        Self {
            width: 0,
            height: 0,
            pixels: Vec::new(),
            textures: HashMap::new(),
            filled: 0,
            spans: 0,
            px_solid_fill: 0,
            px_solid_over: 0,
            px_grad_row: 0,
            px_grad: 0,
            px_tex_linear: 0,
            px_tex_nearest: 0,
        }
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn pixels(&self) -> &[Texel] {
        &self.pixels
    }

    pub fn texture_limit(&self) -> usize {
        8192
    }

    /// Write the canvas as a 32-bit BMP so the rendering can be inspected.
    pub fn save_bmp(&self, path: &std::path::Path) -> std::io::Result<()> {
        use std::io::Write as _;
        let row_bytes = self.width as usize * 4;
        let image_bytes = row_bytes * self.height as usize;
        let mut file = std::fs::File::create(path)?;
        let mut header = Vec::with_capacity(54);
        header.extend_from_slice(b"BM");
        header.extend_from_slice(&((54 + image_bytes) as u32).to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(&54u32.to_le_bytes());
        header.extend_from_slice(&40u32.to_le_bytes());
        header.extend_from_slice(&(self.width as i32).to_le_bytes());
        header.extend_from_slice(&(self.height as i32).to_le_bytes());
        header.extend_from_slice(&1u16.to_le_bytes());
        header.extend_from_slice(&32u16.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(&(image_bytes as u32).to_le_bytes());
        header.extend_from_slice(&2835i32.to_le_bytes());
        header.extend_from_slice(&2835i32.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        file.write_all(&header)?;
        // BM files are bottom-up; the canvas is top-down.
        for y in (0..self.height as usize).rev() {
            let row = &self.pixels[y * self.width as usize..][..self.width as usize];
            let bytes = unsafe { std::slice::from_raw_parts(row.as_ptr().cast::<u8>(), row_bytes) };
            file.write_all(bytes)?;
        }
        file.flush()
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        let width = width.max(1);
        let height = height.max(1);
        if width == self.width && height == self.height {
            return;
        }
        self.width = width;
        self.height = height;
        self.pixels = vec![0; (width as usize) * (height as usize)];
    }

    /// Fill the whole surface with an opaque colour.
    pub fn clear(&mut self, color: Color32) {
        let value = pack(color.r(), color.g(), color.b(), 255);
        self.pixels.fill(value);
        self.filled = 0;
    }

    pub fn apply_textures(&mut self, delta: &TexturesDelta) {
        let profile = gfx_trace() != 0;
        for (id, image_delta) in &delta.set {
            let started = std::time::Instant::now();
            let image = match &image_delta.image {
                egui::ImageData::Color(image) => image,
            };
            match image_delta.pos {
                None => {
                    self.textures.insert(
                        *id,
                        Arc::new(Texture::from_image(image, image_delta.options)),
                    );
                }
                Some(pos) => {
                    if let Some(texture) = self.textures.get_mut(id) {
                        Arc::make_mut(texture).update(image, pos);
                    } else {
                        self.textures.insert(
                            *id,
                            Arc::new(Texture::from_image(image, image_delta.options)),
                        );
                    }
                }
            }
            if profile {
                eprintln!(
                    "gfx upload {id:?}: {}x{} at {:?}, {:.2} ms",
                    image.width(),
                    image.height(),
                    image_delta.pos,
                    started.elapsed().as_secs_f64() * 1000.0,
                );
            }
        }
        for id in &delta.free {
            self.textures.remove(id);
        }
    }

    pub fn rasterize(&mut self, primitives: &[ClippedPrimitive], pixels_per_point: f32) {
        let profile = gfx_trace() >= 2;
        for (index, primitive) in primitives.iter().enumerate() {
            let clip = self.clip_rect(primitive.clip_rect, pixels_per_point);
            if clip.is_empty() {
                continue;
            }
            if let Primitive::Mesh(mesh) = &primitive.primitive {
                let texture = self.textures.get(&mesh.texture_id).cloned();
                let started = std::time::Instant::now();
                let before = (
                    self.filled,
                    self.spans,
                    self.px_solid_fill,
                    self.px_solid_over,
                    self.px_grad_row,
                    self.px_grad,
                    self.px_tex_linear,
                    self.px_tex_nearest,
                );
                self.mesh(mesh, texture.as_deref(), clip, pixels_per_point);
                if profile {
                    eprintln!(
                        "gfx prim {index}: {} tris, {} px, {:.2} ms (texture {:?}) spans {} solid {}/{} grad {}/{} tex {}/{}",
                        mesh.indices.len() / 3,
                        self.filled - before.0,
                        started.elapsed().as_secs_f64() * 1000.0,
                        mesh.texture_id,
                        self.spans - before.1,
                        self.px_solid_fill - before.2,
                        self.px_solid_over - before.3,
                        self.px_grad_row - before.4,
                        self.px_grad - before.5,
                        self.px_tex_linear - before.6,
                        self.px_tex_nearest - before.7,
                    );
                }
            }
        }
    }

    fn clip_rect(&self, rect: egui::Rect, pixels_per_point: f32) -> Clip {
        let x0 = (rect.min.x * pixels_per_point).floor().max(0.0) as i32;
        let y0 = (rect.min.y * pixels_per_point).floor().max(0.0) as i32;
        let x1 = (rect.max.x * pixels_per_point)
            .ceil()
            .min(self.width as f32) as i32;
        let y1 = (rect.max.y * pixels_per_point)
            .ceil()
            .min(self.height as f32) as i32;
        Clip { x0, y0, x1, y1 }
    }

    fn mesh(&mut self, mesh: &Mesh, texture: Option<&Texture>, clip: Clip, scale: f32) {
        let vertices = &mesh.vertices;
        if vertices.is_empty() {
            return;
        }
        let mut index = 0;
        while index + 2 < mesh.indices.len() {
            let i0 = mesh.indices[index] as usize;
            let i1 = mesh.indices[index + 1] as usize;
            let i2 = mesh.indices[index + 2] as usize;
            index += 3;
            if i0 < vertices.len() && i1 < vertices.len() && i2 < vertices.len() {
                self.triangle(
                    &vertices[i0],
                    &vertices[i1],
                    &vertices[i2],
                    texture,
                    clip,
                    scale,
                );
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn triangle(
        &mut self,
        va: &Vertex,
        vb: &Vertex,
        vc: &Vertex,
        texture: Option<&Texture>,
        clip: Clip,
        scale: f32,
    ) {
        let mut pos = [
            [va.pos.x * scale, va.pos.y * scale],
            [vb.pos.x * scale, vb.pos.y * scale],
            [vc.pos.x * scale, vc.pos.y * scale],
        ];
        let mut col = [va.color, vb.color, vc.color];
        let mut uv = [va.uv, vb.uv, vc.uv];

        let (mut e1, mut e2) = edges(pos);
        let mut area = e1[0] * e2[1] - e1[1] * e2[0];
        if area.abs() < 1e-4 {
            return;
        }
        if area < 0.0 {
            // Normalise the winding (counter-clockwise) so the span maths below
            // always sees a positive area.
            pos.swap(1, 2);
            col.swap(1, 2);
            uv.swap(1, 2);
            (e1, e2) = edges(pos);
            area = -area;
        }
        let inv_area = 1.0 / area;

        let top = pos[0][1].min(pos[1][1]).min(pos[2][1]);
        let bottom = pos[0][1].max(pos[1][1]).max(pos[2][1]);
        let y_start = ((top - 0.5).ceil() as i32).max(clip.y0);
        let y_end = ((bottom - 0.5).floor() as i32).min(clip.y1 - 1);
        if y_start > y_end {
            return;
        }

        // Edge crossings: x = x0 + (y - y0) * slope.
        let mut slopes = [[0.0f32; 4]; 3]; // x0, y0, slope, end y
        let mut horizontal = [true; 3];
        for index in 0..3 {
            let next = (index + 1) % 3;
            let (x0, y0) = (pos[index][0], pos[index][1]);
            let (x1, y1) = (pos[next][0], pos[next][1]);
            let dy = y1 - y0;
            if dy.abs() > 1e-6 {
                slopes[index] = [x0, y0, (x1 - x0) / dy, y1];
                horizontal[index] = false;
            }
        }

        // egui hands out WHITE_UV for every untextured triangle: all three uvs
        // are identical, so a sampler would fetch the same texel for every pixel
        // of the span. Fold that texel into the vertex colours once and fall
        // through to the much cheaper solid/gradient kernels.
        let mut texture = texture;
        if let Some(tex) = texture {
            if uv[0] == uv[1] && uv[1] == uv[2] {
                let x = (uv[0].x * tex.width as f32)
                    .floor()
                    .clamp(0.0, tex.width as f32 - 1.0) as usize;
                let y = (uv[0].y * tex.height as f32)
                    .floor()
                    .clamp(0.0, tex.height as f32 - 1.0) as usize;
                let texel = tex.pixels[y * tex.width + x];
                let texel = [
                    (texel >> 16) & 0xFF,
                    (texel >> 8) & 0xFF,
                    texel & 0xFF,
                    (texel >> 24) & 0xFF,
                ];
                for colour in &mut col {
                    let [r, g, b, a] = colour.to_array();
                    *colour = egui::Color32::from_rgba_premultiplied(
                        mul255(r as u32, texel[0]) as u8,
                        mul255(g as u32, texel[1]) as u8,
                        mul255(b as u32, texel[2]) as u8,
                        mul255(a as u32, texel[3]) as u8,
                    );
                }
                texture = None;
            }
        }

        let same = col[0] == col[1] && col[1] == col[2];
        let [r0, g0, b0, a0] = col[0].to_array();
        let shade = if let Some(texture) = texture {
            Shade::Textured {
                texture,
                u0: [uv[0].x, uv[0].y],
                u1: [uv[1].x, uv[1].y],
                u2: [uv[2].x, uv[2].y],
                tint: same.then_some([r0 as u32, g0 as u32, b0 as u32, a0 as u32]),
            }
        } else if same {
            Shade::Solid {
                r: r0 as u32,
                g: g0 as u32,
                b: b0 as u32,
                a: a0 as u32,
            }
        } else {
            let p0 = col[0].to_array();
            let p1 = col[1].to_array();
            let p2 = col[2].to_array();
            let c0 = [p0[0] as f32, p0[1] as f32, p0[2] as f32, p0[3] as f32];
            let c1 = [p1[0] as f32, p1[1] as f32, p1[2] as f32, p1[3] as f32];
            let c2 = [p2[0] as f32, p2[1] as f32, p2[2] as f32, p2[3] as f32];
            Shade::Gradient(c0, c1, c2)
        };

        // Plane equations for the interpolated attributes: q(p) = q0 + A * dx + B * dy.
        let (c0, c1, c2) = (
            [
                col[0].r() as f32,
                col[0].g() as f32,
                col[0].b() as f32,
                col[0].a() as f32,
            ],
            [
                col[1].r() as f32,
                col[1].g() as f32,
                col[1].b() as f32,
                col[1].a() as f32,
            ],
            [
                col[2].r() as f32,
                col[2].g() as f32,
                col[2].b() as f32,
                col[2].a() as f32,
            ],
        );
        let (u0, u1, u2) = match &shade {
            Shade::Textured { u0, u1, u2, .. } => (*u0, *u1, *u2),
            _ => ([0.0; 2], [0.0; 2], [0.0; 2]),
        };
        let px0 = pos[0][0];
        let py0 = pos[0][1];
        if triangle_debug() {
            static COUNT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
            let index = COUNT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if index < 40 {
                eprintln!(
                    "tri {index}: p={pos:?} col={:?} area={area:.1} y={y_start}..{y_end}",
                    col.map(|c| c.to_array()),
                );
            }
        }

        for y in y_start..=y_end {
            let yc = y as f32 + 0.5;
            let mut xmin = f32::INFINITY;
            let mut xmax = f32::NEG_INFINITY;
            for (index, edge) in slopes.iter().enumerate() {
                if horizontal[index] {
                    continue;
                }
                let (ex, ey, slope, end_y) = (edge[0], edge[1], edge[2], edge[3]);
                if yc < ey.min(end_y) || yc > ey.max(end_y) {
                    continue;
                }
                let x = ex + (yc - ey) * slope;
                if x < xmin {
                    xmin = x;
                }
                if x > xmax {
                    xmax = x;
                }
            }
            if !(xmin <= xmax) {
                continue;
            }
            let xs = ((xmin - 0.5).ceil() as i32).max(clip.x0);
            let xe = ((xmax - 0.5).floor() as i32).min(clip.x1 - 1);
            if xs > xe {
                continue;
            }
            let row = y as usize * self.width as usize;
            let length = (xe - xs + 1) as usize;
            self.filled += length as u64;
            self.spans += 1;
            let dx = xs as f32 + 0.5 - px0;
            let dy = yc - py0;
            // Attribute values at the leftmost pixel, plus their x increments.
            macro_rules! attrs {
                ($values:expr, $count:expr) => {{
                    let values = $values;
                    let mut at = [0.0f32; $count];
                    let mut step = [0.0f32; $count];
                    for channel in 0..$count {
                        let a = values.0[channel];
                        let b = values.1[channel];
                        let c = values.2[channel];
                        let da = ((b - a) * e2[1] - (c - a) * e1[1]) * inv_area;
                        let db = ((c - a) * e1[0] - (b - a) * e2[0]) * inv_area;
                        at[channel] = a + da * dx + db * dy;
                        step[channel] = da;
                    }
                    (at, step)
                }};
            }
            match &shade {
                Shade::Solid { r, g, b, a } => {
                    let start = row + xs as usize;
                    let end = start + length;
                    let slice = &mut self.pixels[start..end];
                    if *a == 0 {
                        continue;
                    }
                    if *a == 255 {
                        self.px_solid_fill += length as u64;
                        let value = pack(*r as u8, *g as u8, *b as u8, 255);
                        slice.fill(value);
                    } else {
                        self.px_solid_over += length as u64;
                        let inv = 255 - *a;
                        for pixel in slice.iter_mut() {
                            *pixel = over(*pixel, *r, *g, *b, inv);
                        }
                    }
                }
                Shade::Gradient(c0, c1, c2) => {
                    let (mut at, step) = attrs!((c0, c1, c2), 4);
                    let start = row + xs as usize;
                    let end = start + length;
                    // A purely vertical gradient (the window chassis) has no
                    // horizontal variation: one colour per row, so the span
                    // collapses into a fill instead of per-pixel blending.
                    if step[0].abs() < 1e-4
                        && step[1].abs() < 1e-4
                        && step[2].abs() < 1e-4
                        && step[3].abs() < 1e-4
                    {
                        let a = at[3].clamp(0.0, 255.0);
                        let sa = (a + 0.5) as u32;
                        if sa == 0 {
                            continue;
                        }
                        let r = (at[0].clamp(0.0, a) + 0.5) as u32;
                        let g = (at[1].clamp(0.0, a) + 0.5) as u32;
                        let b = (at[2].clamp(0.0, a) + 0.5) as u32;
                        self.px_grad_row += length as u64;
                        let slice = &mut self.pixels[start..end];
                        if sa >= 255 {
                            slice.fill(pack(r as u8, g as u8, b as u8, 255));
                        } else {
                            let inv = 255 - sa;
                            for pixel in slice.iter_mut() {
                                *pixel = over(*pixel, r, g, b, inv);
                            }
                        }
                        continue;
                    }
                    self.px_grad += length as u64;
                    for pixel in &mut self.pixels[start..end] {
                        let a = at[3];
                        if a > 0.5 {
                            let sa = (a + 0.5) as u32;
                            if sa >= 255 {
                                *pixel = pack(
                                    (at[0].clamp(0.0, a) + 0.5) as u8,
                                    (at[1].clamp(0.0, a) + 0.5) as u8,
                                    (at[2].clamp(0.0, a) + 0.5) as u8,
                                    255,
                                );
                            } else {
                                let inv = 255 - sa;
                                *pixel = over(
                                    *pixel,
                                    (at[0].clamp(0.0, a) + 0.5).min(255.0) as u32,
                                    (at[1].clamp(0.0, a) + 0.5).min(255.0) as u32,
                                    (at[2].clamp(0.0, a) + 0.5).min(255.0) as u32,
                                    inv,
                                );
                            }
                        }
                        for channel in 0..4 {
                            at[channel] += step[channel];
                        }
                    }
                }
                Shade::Textured { texture, tint, .. } => {
                    let (at, step) = attrs!((u0, u1, u2), 2);
                    let start = row + xs as usize;
                    let end = start + length;
                    let width = texture.width as f32;
                    let height = texture.height as f32;
                    let linear = texture.linear;
                    let pixels = &texture.pixels;
                    let texture_width = texture.width;
                    let texture_height = texture.height;
                    let step_x = step[0] * width;
                    let step_y = step[1] * height;
                    let mut fx = at[0] * width - 0.5;
                    let mut fy = at[1] * height - 0.5;
                    let tint = *tint;
                    let slice = &mut self.pixels[start..end];
                    // The common case (wheel, glow, slider, brand, glyph masks)
                    // is a white vertex colour, so the texel passes through
                    // untouched and the sampler can write the packed pixel.
                    if tint == Some([255, 255, 255, 255]) {
                        if linear {
                            self.px_tex_linear += length as u64;
                            for pixel in slice.iter_mut() {
                                let src = bilinear(pixels, texture_width, texture_height, fx, fy);
                                blend_packed(pixel, src);
                                fx += step_x;
                                fy += step_y;
                            }
                        } else {
                            self.px_tex_nearest += length as u64;
                            for pixel in slice.iter_mut() {
                                let src = nearest(pixels, texture_width, texture_height, fx, fy);
                                blend_packed(pixel, src);
                                fx += step_x;
                                fy += step_y;
                            }
                        }
                        continue;
                    }
                    let mut co = match tint {
                        Some(tint) => [
                            tint[0] as f32,
                            tint[1] as f32,
                            tint[2] as f32,
                            tint[3] as f32,
                        ],
                        None => [0.0; 4],
                    };
                    let (_, cstep) = match tint {
                        Some(_) => ([0.0f32; 4], [0.0f32; 4]),
                        None => attrs!((c0, c1, c2), 4),
                    };
                    if linear {
                        self.px_tex_linear += length as u64;
                    } else {
                        self.px_tex_nearest += length as u64;
                    }
                    for pixel in slice.iter_mut() {
                        let texel = if linear {
                            sample_bilinear(pixels, texture_width, texture_height, fx, fy, tint)
                        } else {
                            sample_nearest(pixels, texture_width, texture_height, fx, fy, tint)
                        };
                        // Premultiplied vertex colour * premultiplied texel.
                        let src = if tint.is_none() {
                            [
                                mul255(texel[0], co[0].clamp(0.0, 255.0) as u32),
                                mul255(texel[1], co[1].clamp(0.0, 255.0) as u32),
                                mul255(texel[2], co[2].clamp(0.0, 255.0) as u32),
                                mul255(texel[3], co[3].clamp(0.0, 255.0) as u32),
                            ]
                        } else {
                            texel
                        };
                        let a = src[3];
                        if a != 0 {
                            if a >= 255 {
                                *pixel = pack(src[0] as u8, src[1] as u8, src[2] as u8, 255);
                            } else {
                                let inv = 255 - a;
                                *pixel = over(*pixel, src[0], src[1], src[2], inv);
                            }
                        }
                        fx += step_x;
                        fy += step_y;
                        for channel in 0..4 {
                            co[channel] += cstep[channel];
                        }
                    }
                }
            }
        }
    }
}

/// Rasterizer diagnostics level from `COLORMS_GFX_TRACE`: 1 = per-frame,
/// 2 = per-primitive, 3 = per-triangle.
#[inline]
fn gfx_trace() -> usize {
    static LEVEL: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *LEVEL.get_or_init(|| {
        std::env::var("COLORMS_GFX_TRACE")
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    })
}

#[inline]
fn triangle_debug() -> bool {
    gfx_trace() >= 3
}

/// Nearest texel at a (possibly fractional) texel-space coordinate.
#[inline(always)]
fn nearest(pixels: &[Texel], width: usize, height: usize, fx: f32, fy: f32) -> Texel {
    let x = (fx as i32).clamp(0, width as i32 - 1) as usize;
    let y = (fy as i32).clamp(0, height as i32 - 1) as usize;
    pixels[y * width + x]
}

/// Bilinear filtered texel, integer maths on premultiplied channels.
#[inline(always)]
fn bilinear(pixels: &[Texel], width: usize, height: usize, fx: f32, fy: f32) -> Texel {
    let xi = fx as i32;
    let yi = fy as i32;
    let tx = (((fx - xi as f32) * 256.0) as u32).min(256);
    let ty = (((fy - yi as f32) * 256.0) as u32).min(256);
    let x0 = xi.clamp(0, width as i32 - 1) as usize;
    let y0 = yi.clamp(0, height as i32 - 1) as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let p00 = pixels[y0 * width + x0];
    let p01 = pixels[y0 * width + x1];
    let p10 = pixels[y1 * width + x0];
    let p11 = pixels[y1 * width + x1];
    let (itx, ity) = (256 - tx, 256 - ty);
    let channel = |shift: u32| -> u32 {
        let top = ((p00 >> shift) & 0xFF) * itx + ((p01 >> shift) & 0xFF) * tx;
        let bottom = ((p10 >> shift) & 0xFF) * itx + ((p11 >> shift) & 0xFF) * tx;
        (top * ity + bottom * ty) >> 16
    };
    (channel(24) << 24) | (channel(16) << 16) | (channel(8) << 8) | channel(0)
}

/// Source-over a packed premultiplied texel onto a destination pixel.
#[inline(always)]
fn blend_packed(pixel: &mut Texel, src: Texel) {
    let a = src >> 24;
    if a == 0 {
        return;
    }
    if a == 255 {
        *pixel = src;
        return;
    }
    let inv = 255 - a;
    let d = *pixel;
    let r = ((src >> 16) & 0xFF) + (((d >> 16) & 0xFF) * inv + 128 >> 8);
    let g = ((src >> 8) & 0xFF) + (((d >> 8) & 0xFF) * inv + 128 >> 8);
    let b = (src & 0xFF) + ((d & 0xFF) * inv + 128 >> 8);
    let r = r.min(255);
    let g = g.min(255);
    let b = b.min(255);
    *pixel = (255 << 24) | (r << 16) | (g << 8) | b;
}

#[inline(always)]
fn edges(pos: [[f32; 2]; 3]) -> ([f32; 2], [f32; 2]) {
    (
        [pos[1][0] - pos[0][0], pos[1][1] - pos[0][1]],
        [pos[2][0] - pos[0][0], pos[2][1] - pos[0][1]],
    )
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn sample_bilinear(
    pixels: &[Texel],
    width: usize,
    height: usize,
    fx: f32,
    fy: f32,
    tint: Option<[u32; 4]>,
) -> [u32; 4] {
    let xi = fx as i32;
    let yi = fy as i32;
    let tx = (fx - xi as f32).clamp(0.0, 1.0);
    let ty = (fy - yi as f32).clamp(0.0, 1.0);
    let x0 = xi.clamp(0, width as i32 - 1) as usize;
    let y0 = yi.clamp(0, height as i32 - 1) as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let a = pixels[y0 * width + x0];
    let b = pixels[y0 * width + x1];
    let c = pixels[y1 * width + x0];
    let d = pixels[y1 * width + x1];
    let lerp = |a: u32, b: u32, c: u32, d: u32| -> u32 {
        let top = a as f32 + (b as f32 - a as f32) * tx;
        let bottom = c as f32 + (d as f32 - c as f32) * tx;
        (top + (bottom - top) * ty + 0.5) as u32
    };
    let mut out = [
        lerp(
            (a >> 16) & 0xFF,
            (b >> 16) & 0xFF,
            (c >> 16) & 0xFF,
            (d >> 16) & 0xFF,
        ),
        lerp(
            (a >> 8) & 0xFF,
            (b >> 8) & 0xFF,
            (c >> 8) & 0xFF,
            (d >> 8) & 0xFF,
        ),
        lerp(a & 0xFF, b & 0xFF, c & 0xFF, d & 0xFF),
        lerp(
            (a >> 24) & 0xFF,
            (b >> 24) & 0xFF,
            (c >> 24) & 0xFF,
            (d >> 24) & 0xFF,
        ),
    ];
    if let Some([tr, tg, tb, ta]) = tint {
        out[0] = mul255(out[0], tr);
        out[1] = mul255(out[1], tg);
        out[2] = mul255(out[2], tb);
        out[3] = mul255(out[3], ta);
    }
    out
}

#[inline(always)]
#[allow(clippy::too_many_arguments)]
fn sample_nearest(
    pixels: &[Texel],
    width: usize,
    height: usize,
    fx: f32,
    fy: f32,
    tint: Option<[u32; 4]>,
) -> [u32; 4] {
    let x = (fx + 0.5).clamp(0.0, width as f32 - 1.0) as usize;
    let y = (fy + 0.5).clamp(0.0, height as f32 - 1.0) as usize;
    let texel = pixels[y * width + x];
    let mut out = [
        (texel >> 16) & 0xFF,
        (texel >> 8) & 0xFF,
        texel & 0xFF,
        (texel >> 24) & 0xFF,
    ];
    if let Some([tr, tg, tb, ta]) = tint {
        out[0] = mul255(out[0], tr);
        out[1] = mul255(out[1], tg);
        out[2] = mul255(out[2], tb);
        out[3] = mul255(out[3], ta);
    }
    out
}

impl Default for Canvas {
    fn default() -> Self {
        Self::new()
    }
}
