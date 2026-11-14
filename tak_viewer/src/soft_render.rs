//! CPU painter for egui output, used by `--headless` (`headless.rs`) instead of a GPU window.
//! Same math as the window painter (egui_glow 0.29): premultiplied colors in gamma space,
//! vertex color x texel, blend ONE / ONE_MINUS_SRC_ALPHA (alpha: ONE_MINUS_DST_ALPHA / ONE),
//! clip rect rounded to whole pixels, linear (bilinear) or nearest texture filter, clamp to edge.
//! Content matches a window capture; single pixel values can differ a little (rasterization,
//! sRGB texture decoding on the GPU).

use std::collections::HashMap;

use eframe::egui::epaint::textures::TexturesDelta;
use eframe::egui::epaint::{ClippedPrimitive, ImageData, Primitive, TextureId, Vertex};
use eframe::egui::{Color32, ColorImage, TextureFilter};

/// Premultiplied RGBA, 0..1.
type Px = [f32; 4];

struct Texture {
    w: usize,
    h: usize,
    px: Vec<Px>,
    linear: bool,
}

fn px_from(c: Color32) -> Px {
    let [r, g, b, a] = c.to_array();
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, a as f32 / 255.0]
}

/// The textures egui has handed out so far (font atlas, images), kept like a GPU backend would.
#[derive(Default)]
pub struct Textures {
    map: HashMap<TextureId, Texture>,
}

impl Textures {
    /// Apply `delta.set` (whole textures and patches). Call before painting the frame.
    pub fn set(&mut self, delta: &TexturesDelta) {
        for (id, d) in &delta.set {
            let (size, pixels): ([usize; 2], Vec<Color32>) = match &d.image {
                ImageData::Color(img) => (img.size, img.pixels.clone()),
                ImageData::Font(font) => (font.size, font.srgba_pixels(None).collect()),
            };
            let linear = d.options.magnification == TextureFilter::Linear;
            match d.pos {
                None => {
                    let px = pixels.into_iter().map(px_from).collect();
                    self.map.insert(*id, Texture { w: size[0], h: size[1], px, linear });
                }
                Some([x0, y0]) => {
                    let Some(t) = self.map.get_mut(id) else { continue };
                    for (i, c) in pixels.into_iter().enumerate() {
                        let (x, y) = (x0 + i % size[0], y0 + i / size[0]);
                        if x < t.w && y < t.h {
                            t.px[y * t.w + x] = px_from(c);
                        }
                    }
                    t.linear = linear;
                }
            }
        }
    }

    /// Apply `delta.free`. Call after painting the frame.
    pub fn free(&mut self, delta: &TexturesDelta) {
        for id in &delta.free {
            self.map.remove(id);
        }
    }
}

impl Texture {
    fn at(&self, x: i64, y: i64) -> Px {
        let x = x.clamp(0, self.w as i64 - 1) as usize;
        let y = y.clamp(0, self.h as i64 - 1) as usize;
        self.px[y * self.w + x]
    }

    fn sample(&self, u: f32, v: f32) -> Px {
        if self.w == 0 || self.h == 0 {
            return [1.0; 4];
        }
        let (fx, fy) = (u * self.w as f32, v * self.h as f32);
        if !self.linear {
            return self.at(fx.floor() as i64, fy.floor() as i64);
        }
        let (fx, fy) = (fx - 0.5, fy - 0.5);
        let (x0, y0) = (fx.floor(), fy.floor());
        let (tx, ty) = (fx - x0, fy - y0);
        let (x0, y0) = (x0 as i64, y0 as i64);
        let (a, b, c, d) = (self.at(x0, y0), self.at(x0 + 1, y0), self.at(x0, y0 + 1), self.at(x0 + 1, y0 + 1));
        let mut out = [0.0; 4];
        for k in 0..4 {
            let top = a[k] + (b[k] - a[k]) * tx;
            let bottom = c[k] + (d[k] - c[k]) * tx;
            out[k] = top + (bottom - top) * ty;
        }
        out
    }
}

/// Edge function: > 0 when `c` is on the inner side of `a -> b` for a positive triangle.
fn edge(a: [f64; 2], b: [f64; 2], c: [f64; 2]) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Tie rule for pixels exactly on an edge: of two triangles sharing an edge (opposite
/// directions), exactly one takes the pixel, so translucent fills are not blended twice.
fn owns_edge(a: [f64; 2], b: [f64; 2]) -> bool {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    dy < 0.0 || (dy == 0.0 && dx > 0.0)
}

struct Canvas {
    w: usize,
    px: Vec<Px>,
}

impl Canvas {
    fn triangle(&mut self, verts: [&Vertex; 3], ppp: f32, clip: [usize; 4], tex: Option<&Texture>) {
        let p = |v: &Vertex| [(v.pos.x * ppp) as f64, (v.pos.y * ppp) as f64];
        let (mut v, mut q) = (verts, [p(verts[0]), p(verts[1]), p(verts[2])]);
        let mut area = edge(q[0], q[1], q[2]);
        if area.abs() < 1e-12 {
            return;
        }
        if area < 0.0 {
            v.swap(1, 2);
            q.swap(1, 2);
            area = -area;
        }
        let [cx0, cy0, cx1, cy1] = clip;
        let min = |i: usize| q.iter().map(|p| p[i]).fold(f64::INFINITY, f64::min);
        let max = |i: usize| q.iter().map(|p| p[i]).fold(f64::NEG_INFINITY, f64::max);
        let x0 = (min(0).floor().max(0.0) as usize).max(cx0);
        let y0 = (min(1).floor().max(0.0) as usize).max(cy0);
        let x1 = ((max(0).ceil().max(0.0) as usize) + 1).min(cx1);
        let y1 = ((max(1).ceil().max(0.0) as usize) + 1).min(cy1);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let owns = [owns_edge(q[1], q[2]), owns_edge(q[2], q[0]), owns_edge(q[0], q[1])];
        let col = [px_from(v[0].color), px_from(v[1].color), px_from(v[2].color)];
        for y in y0..y1 {
            for x in x0..x1 {
                let c = [x as f64 + 0.5, y as f64 + 0.5];
                let w = [edge(q[1], q[2], c), edge(q[2], q[0], c), edge(q[0], q[1], c)];
                if (0..3).any(|i| w[i] < 0.0 || (w[i] == 0.0 && !owns[i])) {
                    continue;
                }
                let l = [(w[0] / area) as f32, (w[1] / area) as f32, (w[2] / area) as f32];
                let mut src = [0.0f32; 4];
                for k in 0..4 {
                    src[k] = l[0] * col[0][k] + l[1] * col[1][k] + l[2] * col[2][k];
                }
                if let Some(t) = tex {
                    let u = l[0] * v[0].uv.x + l[1] * v[1].uv.x + l[2] * v[2].uv.x;
                    let s = l[0] * v[0].uv.y + l[1] * v[1].uv.y + l[2] * v[2].uv.y;
                    let texel = t.sample(u, s);
                    for k in 0..4 {
                        src[k] *= texel[k];
                    }
                }
                let d = &mut self.px[y * self.w + x];
                let (sa, da) = (src[3], d[3]);
                for k in 0..3 {
                    d[k] = src[k] + d[k] * (1.0 - sa);
                }
                d[3] = sa * (1.0 - da) + da;
            }
        }
    }
}

/// Paint tessellated egui output into a `size_px` image (premultiplied, like a window
/// framebuffer read). `clear` fills the image first.
pub fn paint(prims: &[ClippedPrimitive], textures: &Textures, size_px: [usize; 2], ppp: f32, clear: Color32) -> ColorImage {
    let [w, h] = size_px;
    let mut canvas = Canvas { w, px: vec![px_from(clear); w * h] };
    for prim in prims {
        let Primitive::Mesh(mesh) = &prim.primitive else { continue };
        let r = prim.clip_rect;
        let round = |v: f32, lim: usize| ((v * ppp).round().max(0.0) as usize).min(lim);
        let (cx0, cy0) = (round(r.min.x, w), round(r.min.y, h));
        let (cx1, cy1) = (round(r.max.x, w).max(cx0), round(r.max.y, h).max(cy0));
        if cx0 >= cx1 || cy0 >= cy1 {
            continue;
        }
        let tex = textures.map.get(&mesh.texture_id);
        for tri in mesh.indices.chunks_exact(3) {
            let get = |i: u32| mesh.vertices.get(i as usize);
            let (Some(a), Some(b), Some(c)) = (get(tri[0]), get(tri[1]), get(tri[2])) else { continue };
            canvas.triangle([a, b, c], ppp, [cx0, cy0, cx1, cy1], tex);
        }
    }
    let to8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let pixels = canvas.px.iter().map(|p| Color32::from_rgba_premultiplied(to8(p[0]), to8(p[1]), to8(p[2]), to8(p[3]))).collect();
    ColorImage { size: [w, h], pixels }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{self, pos2, vec2, Rect, RawInput};

    /// One egui frame of `size` points at 1 pixel per point, painted on the CPU.
    fn render(size: [f32; 2], draw: impl FnMut(&egui::Context)) -> ColorImage {
        let ctx = egui::Context::default();
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(size[0], size[1]));
        let mut raw = RawInput { screen_rect: Some(screen), ..Default::default() };
        raw.viewports.entry(egui::ViewportId::ROOT).or_default().native_pixels_per_point = Some(1.0);
        let out = ctx.run(raw, draw);
        let mut textures = Textures::default();
        textures.set(&out.textures_delta);
        let prims = ctx.tessellate(out.shapes, out.pixels_per_point);
        paint(&prims, &textures, [size[0] as usize, size[1] as usize], out.pixels_per_point, Color32::BLACK)
    }

    fn painter(ctx: &egui::Context) -> egui::Painter {
        ctx.layer_painter(egui::LayerId::background())
    }

    #[test]
    fn filled_rect_covers_exact_pixels() {
        let img = render([20.0, 10.0], |ctx| {
            painter(ctx).rect_filled(Rect::from_min_max(pos2(5.0, 2.0), pos2(15.0, 8.0)), 0.0, Color32::from_rgb(200, 10, 30));
        });
        let at = |x: usize, y: usize| img.pixels[y * 20 + x];
        assert_eq!(at(10, 5), Color32::from_rgb(200, 10, 30));
        assert_eq!(at(2, 5), Color32::BLACK);
        assert_eq!(at(16, 5), Color32::BLACK);
        // Interior pixels are covered once (no double blend on the quad's diagonal).
        assert!((5..15).all(|x| (2..8).all(|y| at(x, y) == Color32::from_rgb(200, 10, 30))));
    }

    #[test]
    fn translucent_rect_blends_premultiplied_and_clip_cuts() {
        let img = render([20.0, 10.0], |ctx| {
            let p = painter(ctx);
            p.rect_filled(Rect::from_min_max(pos2(0.0, 0.0), pos2(20.0, 10.0)), 0.0, Color32::from_rgb(0, 0, 200));
            let half = Color32::from_rgba_unmultiplied(255, 0, 0, 128);
            p.with_clip_rect(Rect::from_min_max(pos2(0.0, 0.0), pos2(10.0, 10.0)))
                .rect_filled(Rect::from_min_max(pos2(0.0, 0.0), pos2(20.0, 10.0)), 0.0, half);
        });
        let left = img.pixels[5 * 20 + 3].to_array();
        let right = img.pixels[5 * 20 + 15].to_array();
        assert_eq!(right, [0, 0, 200, 255]);
        // egui premultiplies in linear space: red 255 at alpha 128 is stored as about 188.
        let half = Color32::from_rgba_unmultiplied(255, 0, 0, 128).to_array();
        let blue = (200.0 * (1.0 - half[3] as f32 / 255.0)).round() as i32;
        assert!((left[0] as i32 - half[0] as i32).abs() <= 1 && (left[2] as i32 - blue).abs() <= 1, "{left:?}");
    }

    #[test]
    fn text_uses_the_font_texture() {
        let img = render([80.0, 30.0], |ctx| {
            painter(ctx).text(pos2(4.0, 4.0), egui::Align2::LEFT_TOP, "TAK", egui::FontId::proportional(20.0), Color32::WHITE);
        });
        let lit = img.pixels.iter().filter(|p| p.r() > 128).count();
        assert!(lit > 30 && lit < 80 * 30 / 2, "lit pixels {lit}");
    }
}
