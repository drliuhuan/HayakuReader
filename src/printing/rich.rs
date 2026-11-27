//! Native flow layout and a CPU renderer for the same egui meshes used by the reader.
//! Pagination only records slices; PNG pages are rasterized on demand.
use super::Mark;
use egui::{
    ColorImage, Context, Rect, Shape, TextureId, Vec2,
    epaint::{ClippedPrimitive, Primitive},
};
use std::collections::HashMap;

pub(crate) fn bold_ranges(block: &crate::flow::Block) -> Vec<(usize, usize)> {
    use crate::flow::{Block, Inline};
    fn inlines(
        items: &[Inline],
        strong: bool,
        offset: &mut usize,
        ranges: &mut Vec<(usize, usize)>,
    ) {
        for item in items {
            match item {
                Inline::Styled {
                    bold,
                    inlines: inner,
                    ..
                } => inlines(inner, strong || *bold, offset, ranges),
                Inline::Link { inlines: inner, .. } => inlines(inner, strong, offset, ranges),
                _ => {
                    let length = match item {
                        Inline::Text(text) => text.encode_utf16().count(),
                        Inline::Ruby { base, .. } => base.encode_utf16().count(),
                        Inline::InlineMath { latex } => latex.encode_utf16().count(),
                        Inline::LineBreak => 1,
                        _ => 0,
                    };
                    if strong && length > 0 {
                        ranges.push((*offset, *offset + length));
                    }
                    *offset += length;
                }
            }
        }
    }
    fn walk(block: &Block, offset: &mut usize, ranges: &mut Vec<(usize, usize)>) {
        match block {
            Block::Heading { inlines: items, .. } => {
                inlines(items, true, offset, ranges);
                *offset += 1;
            }
            Block::Paragraph(items) => {
                inlines(items, false, offset, ranges);
                *offset += 1;
            }
            Block::Quote(items) => {
                for item in items {
                    walk(item, offset, ranges);
                }
            }
            Block::List { items, .. } => {
                for item in items {
                    for block in item {
                        walk(block, offset, ranges);
                    }
                }
            }
            Block::Table { headers, rows } => {
                for cell in headers {
                    inlines(cell, true, offset, ranges);
                    *offset += 1;
                }
                *offset += 1;
                for row in rows {
                    for cell in row {
                        inlines(cell, false, offset, ranges);
                        *offset += 1;
                    }
                    *offset += 1;
                }
            }
            _ => {
                let mut plain = String::new();
                crate::flow::collect_block_plain(block, &mut plain);
                *offset += plain.encode_utf16().count();
            }
        }
    }
    let mut ranges = Vec::new();
    walk(block, &mut 0, &mut ranges);
    ranges
}

pub(crate) struct BlockScene {
    pub chapter: usize,
    pub start: usize,
    pub height: f32,
    pub width: f32,
    pub meshes: Vec<ClippedPrimitive>,
    pub glyphs: Vec<(usize, usize, Rect)>,
    pub occupied: Vec<(f32, f32)>,
}

pub(crate) struct Slice {
    block: usize,
    from: f32,
    to: f32,
    y: f32,
    scale: f32,
}
pub(crate) struct RichPage {
    slices: Vec<Slice>,
    pub marks: Vec<Mark>,
}
pub(crate) struct RichLayout {
    pub blocks: Vec<BlockScene>,
    pub textures: HashMap<TextureId, ColorImage>,
    pub pages: Vec<RichPage>,
    pub width: u32,
    pub height: u32,
}

pub(crate) struct CaptureSpec<'a> {
    pub plain: &'a str,
    pub height: f32,
    pub width: f32,
    pub chapter: usize,
    pub start: usize,
    pub bold: &'a [(usize, usize)],
}

pub(crate) fn capture(
    ctx: &Context,
    mut frame: egui::FullOutput,
    spec: CaptureSpec<'_>,
    textures: &mut HashMap<TextureId, ColorImage>,
    previous_scenes: &mut [BlockScene],
) -> BlockScene {
    let CaptureSpec {
        plain,
        height,
        width,
        chapter,
        start,
        bold,
    } = spec;
    for (id, deltas) in &frame.textures_delta.set {
        for delta in deltas {
            let egui::ImageData::Color(image) = &delta.image;
            if let Some([x, y]) = delta.pos {
                if let Some(destination) = textures.get_mut(id) {
                    for row in 0..image.height() {
                        let offset = (y + row) * destination.width() + x;
                        destination.pixels[offset..offset + image.width()].copy_from_slice(
                            &image.pixels[row * image.width()..(row + 1) * image.width()],
                        );
                    }
                }
            } else {
                // egui rebuilds its font atlas once it is 80% full. Long CJK
                // books routinely trigger this. Cached meshes must retain the
                // atlas they were tessellated against, not the latest glyphs
                // stored under the same Managed texture id.
                if previous_scenes.iter().any(|scene| {
                    scene.meshes.iter().any(|primitive|
                    matches!(&primitive.primitive, Primitive::Mesh(mesh) if mesh.texture_id == *id))
                }) && let Some(previous) = textures.remove(id)
                {
                    let mut generation = u64::MAX;
                    while textures.contains_key(&TextureId::User(generation)) {
                        generation -= 1;
                    }
                    let frozen = TextureId::User(generation);
                    textures.insert(frozen, previous);
                    for scene in previous_scenes.iter_mut() {
                        for primitive in &mut scene.meshes {
                            if let Primitive::Mesh(mesh) = &mut primitive.primitive
                                && mesh.texture_id == *id
                            {
                                mesh.texture_id = frozen;
                            }
                        }
                    }
                }
                textures.insert(*id, image.as_ref().clone());
            }
        }
    }
    let ppp = frame.pixels_per_point;
    let mut glyphs = Vec::new();
    let mut occupied = Vec::new();
    let mut cursor = 0;
    fn visit(
        shape: &mut Shape,
        plain: &str,
        cursor: &mut usize,
        glyphs: &mut Vec<(usize, usize, Rect)>,
        occupied: &mut Vec<(f32, f32)>,
        bold: &[(usize, usize)],
    ) {
        match shape {
            Shape::Vec(shapes) => {
                for shape in shapes {
                    visit(shape, plain, cursor, glyphs, occupied, bold);
                }
            }
            Shape::Text(text) => {
                for row in &text.galley.rows {
                    let rect = row.rect().translate(text.pos.to_vec2());
                    if !row.glyphs.is_empty() {
                        occupied.push((rect.top(), rect.bottom()));
                    }
                }
                let job = &text.galley.job.text;
                if let Some(found) = plain[*cursor..]
                    .find(job.as_str())
                    .filter(|_| !job.is_empty())
                {
                    let offset = *cursor + found;
                    let mut index = plain[..offset].encode_utf16().count();
                    let begin = index;
                    for row in &text.galley.rows {
                        for glyph in &row.glyphs {
                            let end = index + glyph.chr.len_utf16();
                            glyphs.push((
                                index,
                                end,
                                glyph
                                    .logical_rect()
                                    .translate(text.pos.to_vec2() + row.pos.to_vec2()),
                            ));
                            index = end;
                        }
                        if row.ends_with_newline {
                            index += 1;
                        }
                    }
                    *cursor = offset + job.len();
                    // egui's normal proportional face has no weight axis. A small
                    // second stroke preserves strong spans without losing ruby/math.
                    if bold
                        .iter()
                        .any(|&(start, end)| start < index && end > begin)
                    {
                        let mut second = text.clone();
                        second.pos.x += 0.4;
                        *shape = Shape::Vec(vec![Shape::Text(text.clone()), Shape::Text(second)]);
                    }
                }
            }
            Shape::Rect(rect) if rect.brush.is_some() => {
                occupied.push((rect.rect.top(), rect.rect.bottom()))
            }
            Shape::Mesh(mesh) if mesh.texture_id != TextureId::default() => {
                let rect = mesh.calc_bounds();
                occupied.push((rect.top(), rect.bottom()));
            }
            _ => {}
        }
    }
    for shape in &mut frame.shapes {
        visit(
            &mut shape.shape,
            plain,
            &mut cursor,
            &mut glyphs,
            &mut occupied,
            bold,
        );
    }
    let mut meshes = ctx.tessellate(std::mem::take(&mut frame.shapes), ppp);
    for primitive in &mut meshes {
        primitive.clip_rect = primitive.clip_rect * ppp;
        if let Primitive::Mesh(mesh) = &mut primitive.primitive
            && let Some(texture) = textures.get(&mesh.texture_id)
        {
            for vertex in &mut mesh.vertices {
                vertex.pos *= ppp;
                vertex.uv.x *= texture.width() as f32;
                vertex.uv.y *= texture.height() as f32;
            }
        }
    }
    for (_, _, rect) in &mut glyphs {
        *rect = *rect * ppp;
    }
    for (top, bottom) in &mut occupied {
        *top *= ppp;
        *bottom *= ppp;
    }
    frame.drop_without_applying_deltas();
    BlockScene {
        chapter,
        start,
        height: height * ppp,
        width: width * ppp,
        meshes,
        glyphs,
        occupied,
    }
}

impl RichLayout {
    pub fn paginate(
        blocks: Vec<BlockScene>,
        textures: HashMap<TextureId, ColorImage>,
        width: u32,
        height: u32,
        chapter_marks: &[Vec<Mark>],
    ) -> Self {
        let mut pages = Vec::new();
        let mut slices = Vec::new();
        let mut y = 0.0;
        for (index, block) in blocks.iter().enumerate() {
            if block.height <= 0.0 {
                continue;
            }
            let mut occupied = block.occupied.clone();
            occupied.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut cuts = Vec::new();
            let mut bottom = 0.0_f32;
            for (top, next_bottom) in occupied {
                if top >= bottom - 0.01 && top > 0.1 {
                    cuts.push((top + bottom) * 0.5);
                }
                bottom = bottom.max(next_bottom);
            }
            cuts.push(block.height);
            let mut from = 0.0;
            while from < block.height - 0.1 {
                let remaining = height as f32 - y;
                let limit = from + remaining;
                let to = cuts
                    .iter()
                    .copied()
                    .filter(|&cut| cut > from + 0.1 && cut <= limit + 0.1)
                    .max_by(f32::total_cmp);
                if let Some(to) = to {
                    slices.push(Slice {
                        block: index,
                        from,
                        to,
                        y,
                        scale: 1.0,
                    });
                    y += to - from;
                    from = to;
                } else if slices.is_empty() {
                    // A single illustration or equation is kept intact and fitted.
                    let to = cuts
                        .iter()
                        .copied()
                        .find(|&cut| cut > from + 0.1)
                        .unwrap_or(block.height);
                    let scale = (height as f32 / (to - from)).min(1.0);
                    slices.push(Slice {
                        block: index,
                        from,
                        to,
                        y: 0.0,
                        scale,
                    });
                    y = (to - from) * scale;
                    from = to;
                }
                if from < block.height - 0.1 || y >= height as f32 - 0.1 {
                    pages.push(RichPage {
                        slices: std::mem::take(&mut slices),
                        marks: Vec::new(),
                    });
                    y = 0.0;
                }
            }
        }
        if !slices.is_empty() || pages.is_empty() {
            pages.push(RichPage {
                slices,
                marks: Vec::new(),
            });
        }
        for page in &mut pages {
            for slice in &page.slices {
                let block = &blocks[slice.block];
                for mark in chapter_marks.get(block.chapter).into_iter().flatten() {
                    let mut rects = Vec::new();
                    for &(start, end, rect) in &block.glyphs {
                        if mark.start < block.start + end
                            && mark.end > block.start + start
                            && rect.center().y >= slice.from
                            && rect.center().y < slice.to
                        {
                            rects.push([
                                rect.left() * slice.scale,
                                (rect.top() - slice.from) * slice.scale + slice.y,
                                rect.width() * slice.scale,
                                rect.height() * slice.scale,
                            ]);
                        }
                    }
                    if !rects.is_empty() {
                        if let Some(existing) =
                            page.marks.iter_mut().find(|other| other.key == mark.key)
                        {
                            existing.rects.extend(rects);
                        } else {
                            let mut mark = mark.clone();
                            mark.rects = rects;
                            page.marks.push(mark);
                        }
                    }
                }
            }
        }
        Self {
            blocks,
            textures,
            pages,
            width,
            height,
        }
    }

    pub fn render(&self, page: usize) -> Result<image::RgbaImage, String> {
        let page = self.pages.get(page).ok_or("打印页码超出范围")?;
        let mut output = image::RgbaImage::from_pixel(
            self.width,
            self.height,
            image::Rgba([255, 255, 255, 255]),
        );
        for slice in &page.slices {
            let block = &self.blocks[slice.block];
            let shift = Vec2::new(
                (self.width as f32 - block.width * slice.scale) * 0.5,
                slice.y - slice.from * slice.scale,
            );
            let slice_clip = Rect::from_min_max(
                egui::pos2(0.0, slice.y),
                egui::pos2(
                    self.width as f32,
                    slice.y + (slice.to - slice.from) * slice.scale,
                ),
            );
            for clipped in &block.meshes {
                let Primitive::Mesh(mesh) = &clipped.primitive else {
                    continue;
                };
                let Some(texture) = self.textures.get(&mesh.texture_id) else {
                    continue;
                };
                let clip = Rect::from_min_max(
                    (clipped.clip_rect.min * slice.scale) + shift,
                    (clipped.clip_rect.max * slice.scale) + shift,
                )
                .intersect(slice_clip);
                for triangle in mesh.indices.as_chunks::<3>().0 {
                    let vertices = triangle
                        .iter()
                        .map(|&index| mesh.vertices[index as usize])
                        .collect::<Vec<_>>();
                    raster_triangle(
                        &mut output,
                        texture,
                        [&vertices[0], &vertices[1], &vertices[2]],
                        slice.scale,
                        shift,
                        clip,
                    );
                }
            }
        }
        Ok(output)
    }
}

fn raster_triangle(
    output: &mut image::RgbaImage,
    texture: &ColorImage,
    vertices: [&egui::epaint::Vertex; 3],
    scale: f32,
    shift: Vec2,
    clip: Rect,
) {
    let mut p = vertices.map(|vertex| vertex.pos * scale + shift);
    let mut v = vertices;
    let edge = |a: egui::Pos2, b: egui::Pos2, c: egui::Pos2| {
        (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x)
    };
    let mut area = edge(p[0], p[1], p[2]);
    if area.abs() < 0.00001 {
        return;
    }
    if area < 0.0 {
        p.swap(1, 2);
        v.swap(1, 2);
        area = -area;
    }
    let bounds = Rect::from_points(&p)
        .intersect(clip)
        .intersect(Rect::from_min_size(
            egui::Pos2::ZERO,
            Vec2::new(output.width() as f32, output.height() as f32),
        ));
    if !bounds.is_positive() {
        return;
    }
    let sample = |x: f32, y: f32| {
        let x = (x - 0.5).clamp(0.0, texture.width() as f32 - 1.0);
        let y = (y - 0.5).clamp(0.0, texture.height() as f32 - 1.0);
        let x0 = x.floor() as usize;
        let y0 = y.floor() as usize;
        let x1 = (x0 + 1).min(texture.width() - 1);
        let y1 = (y0 + 1).min(texture.height() - 1);
        let fx = x - x0 as f32;
        let fy = y - y0 as f32;
        let colors = [
            texture.pixels[y0 * texture.width() + x0],
            texture.pixels[y0 * texture.width() + x1],
            texture.pixels[y1 * texture.width() + x0],
            texture.pixels[y1 * texture.width() + x1],
        ]
        .map(|color| color.to_array());
        std::array::from_fn::<_, 4, _>(|channel| {
            (colors[0][channel] as f32 * (1.0 - fx) + colors[1][channel] as f32 * fx) * (1.0 - fy)
                + (colors[2][channel] as f32 * (1.0 - fx) + colors[3][channel] as f32 * fx) * fy
        })
    };
    for y in bounds.top().floor().max(0.0) as u32
        ..bounds.bottom().ceil().min(output.height() as f32) as u32
    {
        for x in bounds.left().floor().max(0.0) as u32
            ..bounds.right().ceil().min(output.width() as f32) as u32
        {
            let point = egui::pos2(x as f32 + 0.5, y as f32 + 0.5);
            if !clip.contains(point) {
                continue;
            }
            let edges = [
                edge(p[1], p[2], point),
                edge(p[2], p[0], point),
                edge(p[0], p[1], point),
            ];
            let pairs = [(p[1], p[2]), (p[2], p[0]), (p[0], p[1])];
            if edges.iter().zip(pairs).any(|(&value, (a, b))| {
                value < 0.0 || (value == 0.0 && !(b.y > a.y || (b.y == a.y && b.x < a.x)))
            }) {
                continue;
            }
            let weights = edges.map(|value| value / area);
            let uv = v
                .iter()
                .zip(weights)
                .fold(Vec2::ZERO, |sum, (vertex, weight)| {
                    sum + vertex.uv.to_vec2() * weight
                });
            let texel = sample(uv.x, uv.y);
            let color: [f32; 4] = std::array::from_fn(|channel| {
                v.iter()
                    .zip(weights)
                    .map(|(vertex, weight)| vertex.color.to_array()[channel] as f32 * weight)
                    .sum::<f32>()
                    / 255.0
                    * texel[channel]
            });
            let destination = output.get_pixel_mut(x, y);
            for channel in 0..3 {
                destination[channel] = (color[channel]
                    + destination[channel] as f32 * (1.0 - color[3] / 255.0))
                    .clamp(0.0, 255.0)
                    .round() as u8;
            }
        }
    }
}
