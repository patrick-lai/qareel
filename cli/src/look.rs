use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const PALETTE: [(&str, char, [u8; 3]); 22] = [
    ("black", 'k', [18, 18, 18]), ("white", 'w', [242, 242, 242]), ("silver", 's', [190, 190, 190]), ("gray", 'a', [128, 128, 128]), ("charcoal", 'd', [62, 62, 66]),
    ("red", 'r', [222, 44, 44]), ("maroon", 'm', [122, 24, 28]), ("orange", 'o', [245, 140, 30]), ("yellow", 'y', [246, 220, 46]), ("tan", 't', [222, 192, 142]),
    ("brown", 'n', [128, 78, 40]), ("lime", 'l', [150, 224, 70]), ("green", 'g', [48, 160, 64]), ("forest", 'f', [24, 92, 44]), ("teal", 'q', [24, 132, 132]),
    ("cyan", 'c', [48, 204, 222]), ("sky", 'e', [136, 196, 240]), ("blue", 'b', [44, 96, 222]), ("navy", 'v', [22, 32, 108]), ("purple", 'p', [132, 64, 192]),
    ("pink", 'i', [244, 132, 186]), ("magenta", 'h', [212, 44, 164]),
];

#[derive(Clone, Copy, Debug)]
pub struct LookOptions {
    pub columns: u32,
    pub region: Option<(f64, f64, f64, f64)>,
    pub max_objects: usize,
    pub grid: bool,
}

impl Default for LookOptions {
    fn default() -> Self { Self { columns: 64, region: None, max_objects: 24, grid: true } }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct LookObject {
    pub id: u32,
    pub color: String,
    pub x: f64,
    pub y: f64,
    pub left: f64,
    pub top: f64,
    pub width: f64,
    pub height: f64,
    pub share: f64,
}

#[derive(Clone, Debug)]
pub struct Look {
    pub text: String,
    pub objects: Vec<LookObject>,
}

fn nearest(pixel: [f64; 3]) -> usize {
    let mut best = (0, f64::MAX);
    for (index, (_, _, color)) in PALETTE.iter().enumerate() {
        let (dr, dg, db) = (pixel[0] - f64::from(color[0]), pixel[1] - f64::from(color[1]), pixel[2] - f64::from(color[2]));
        let mean = (pixel[0] + f64::from(color[0])) / 2.0;
        let distance = (2.0 + mean / 256.0) * dr * dr + 4.0 * dg * dg + (2.0 + (255.0 - mean) / 256.0) * db * db;
        if distance < best.1 { best = (index, distance); }
    }
    best.0
}

fn cells(image: &image::RgbaImage, left: u32, top: u32, width: u32, height: u32, columns: u32, rows: u32) -> Vec<usize> {
    let mut out = Vec::with_capacity((columns * rows) as usize);
    for row in 0..rows {
        for column in 0..columns {
            let (x0, x1) = (left + column * width / columns, left + ((column + 1) * width / columns).max(column * width / columns + 1));
            let (y0, y1) = (top + row * height / rows, top + ((row + 1) * height / rows).max(row * height / rows + 1));
            let (mut sum, mut count) = ([0.0f64; 3], 0.0);
            let step_x = ((x1 - x0) / 4).max(1);
            let step_y = ((y1 - y0) / 4).max(1);
            let mut y = y0;
            while y < y1.min(image.height()) {
                let mut x = x0;
                while x < x1.min(image.width()) {
                    let pixel = image.get_pixel(x, y).0;
                    let alpha = f64::from(pixel[3]) / 255.0;
                    for channel in 0..3 { sum[channel] += f64::from(pixel[channel]) * alpha + 255.0 * (1.0 - alpha); }
                    count += 1.0;
                    x += step_x;
                }
                y += step_y;
            }
            out.push(if count > 0.0 { nearest([sum[0] / count, sum[1] / count, sum[2] / count]) } else { 1 });
        }
    }
    out
}

pub fn describe(png: &[u8], viewport: (f64, f64), options: LookOptions, previous: Option<&[LookObject]>) -> Result<Look> {
    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png).context("browser_look.image_invalid: the screenshot could not be decoded")?.to_rgba8();
    if image.width() == 0 || image.height() == 0 || viewport.0 <= 0.0 || viewport.1 <= 0.0 { bail!("browser_look.image_invalid: empty screenshot"); }
    let scale = f64::from(image.width()) / viewport.0;
    let (region_left, region_top, region_width, region_height) = match options.region {
        Some((x, y, w, h)) => {
            let left = (x.max(0.0) * scale).floor().min(f64::from(image.width() - 1));
            let top = (y.max(0.0) * scale).floor().min(f64::from(image.height() - 1));
            let width = (w * scale).ceil().clamp(1.0, f64::from(image.width()) - left);
            let height = (h * scale).ceil().clamp(1.0, f64::from(image.height()) - top);
            (left as u32, top as u32, width as u32, height as u32)
        }
        None => (0, 0, image.width(), image.height()),
    };
    let css = |pixels: f64| pixels / scale;
    let columns = options.columns.clamp(8, 160);
    let rows = ((f64::from(columns) * f64::from(region_height) / f64::from(region_width)) / 2.0).round().clamp(2.0, 100.0) as u32;
    let mut text = format!("### Look\n- Area: {:.0},{:.0} {:.0}x{:.0} CSS px of a {:.0}x{:.0} viewport\n", css(f64::from(region_left)), css(f64::from(region_top)), css(f64::from(region_width)), css(f64::from(region_height)), viewport.0, viewport.1);
    if options.grid {
        let grid = cells(&image, region_left, region_top, region_width, region_height, columns, rows);
        let mut used: Vec<usize> = grid.clone();
        used.sort_unstable();
        used.dedup();
        let legend: Vec<String> = used.iter().map(|index| format!("{}={}", PALETTE[*index].1, PALETTE[*index].0)).collect();
        text.push_str(&format!("- Grid: {columns}x{rows}, one character is {:.0}x{:.0} CSS px; legend {}\n```\n", css(f64::from(region_width)) / f64::from(columns), css(f64::from(region_height)) / f64::from(rows), legend.join(" ")));
        for row in grid.chunks(columns as usize) { text.push_str(&row.iter().map(|index| PALETTE[*index].1).collect::<String>()); text.push('\n'); }
        text.push_str("```\n");
    }
    let detect_columns = 160u32.min(region_width);
    let detect_rows = ((f64::from(detect_columns) * f64::from(region_height) / f64::from(region_width)).round() as u32).clamp(2, 120).min(region_height);
    let labels = cells(&image, region_left, region_top, region_width, region_height, detect_columns, detect_rows);
    let total = (detect_columns * detect_rows) as usize;
    let mut seen = vec![false; total];
    let mut components: Vec<(usize, usize, u32, u32, u32, u32, f64, f64)> = Vec::new();
    for start in 0..total {
        if seen[start] { continue; }
        let color = labels[start];
        let (mut stack, mut size, mut min_x, mut min_y, mut max_x, mut max_y, mut sum_x, mut sum_y) = (vec![start], 0usize, u32::MAX, u32::MAX, 0u32, 0u32, 0.0, 0.0);
        seen[start] = true;
        while let Some(cell) = stack.pop() {
            let (x, y) = ((cell as u32) % detect_columns, (cell as u32) / detect_columns);
            size += 1;
            min_x = min_x.min(x); max_x = max_x.max(x); min_y = min_y.min(y); max_y = max_y.max(y);
            sum_x += f64::from(x) + 0.5; sum_y += f64::from(y) + 0.5;
            let mut visit = |nx: u32, ny: u32| { let next = (ny * detect_columns + nx) as usize; if !seen[next] && labels[next] == color { seen[next] = true; stack.push(next); } };
            if x > 0 { visit(x - 1, y); }
            if x + 1 < detect_columns { visit(x + 1, y); }
            if y > 0 { visit(x, y - 1); }
            if y + 1 < detect_rows { visit(x, y + 1); }
        }
        components.push((color, size, min_x, min_y, max_x, max_y, sum_x / size as f64, sum_y / size as f64));
    }
    let cell_width = css(f64::from(region_width)) / f64::from(detect_columns);
    let cell_height = css(f64::from(region_height)) / f64::from(detect_rows);
    let origin = (css(f64::from(region_left)), css(f64::from(region_top)));
    let shape = |component: &(usize, usize, u32, u32, u32, u32, f64, f64)| LookObject {
        id: 0,
        color: PALETTE[component.0].0.to_owned(),
        x: (origin.0 + component.6 * cell_width).round(),
        y: (origin.1 + component.7 * cell_height).round(),
        left: (origin.0 + f64::from(component.2) * cell_width).round(),
        top: (origin.1 + f64::from(component.3) * cell_height).round(),
        width: (f64::from(component.4 - component.2 + 1) * cell_width).round(),
        height: (f64::from(component.5 - component.3 + 1) * cell_height).round(),
        share: (component.1 as f64 * 1000.0 / total as f64).round() / 10.0,
    };
    let mut regions: Vec<_> = components.iter().filter(|component| component.1 * 100 >= total * 8).collect();
    regions.sort_by_key(|component| std::cmp::Reverse(component.1));
    let mut objects: Vec<LookObject> = components.iter().filter(|component| component.1 >= 3 && component.1 * 100 < total * 8 && component.4 > component.2 && component.5 > component.3).map(shape).collect();
    objects.sort_by(|left, right| right.share.partial_cmp(&left.share).unwrap_or(std::cmp::Ordering::Equal));
    objects.truncate(options.max_objects.clamp(1, 80));
    let mut used_ids: Vec<u32> = Vec::new();
    let reach = (viewport.0.max(viewport.1)) * 0.3;
    let mut motion: Vec<Option<(f64, f64)>> = vec![None; objects.len()];
    if let Some(previous) = previous {
        for (index, object) in objects.iter_mut().enumerate() {
            let matched = previous.iter().filter(|old| old.color == object.color && !used_ids.contains(&old.id)).map(|old| (old, (old.x - object.x).hypot(old.y - object.y))).filter(|(old, distance)| *distance <= reach && (old.share - object.share).abs() <= old.share.max(object.share) * 0.6 + 0.2).min_by(|left, right| left.1.partial_cmp(&right.1).unwrap_or(std::cmp::Ordering::Equal));
            if let Some((old, _)) = matched {
                object.id = old.id;
                used_ids.push(old.id);
                motion[index] = Some((object.x - old.x, object.y - old.y));
            }
        }
    }
    let mut next = previous.into_iter().flatten().map(|old| old.id).max().unwrap_or(0);
    for object in objects.iter_mut().filter(|object| object.id == 0) { next += 1; object.id = next; }
    text.push_str("### Objects (id colour centre x,y; box left,top width x height; share of area)\n");
    if objects.is_empty() { text.push_str("- none found\n"); }
    for (object, moved) in objects.iter().zip(&motion) {
        let change = match (previous, moved) {
            (Some(_), Some((dx, dy))) if dx.abs() >= 1.0 || dy.abs() >= 1.0 => format!(" moved {dx:+.0},{dy:+.0}"),
            (Some(_), Some(_)) => " still".to_owned(),
            (Some(_), None) => " new".to_owned(),
            _ => String::new(),
        };
        text.push_str(&format!("- o{} {} {:.0},{:.0} box {:.0},{:.0} {:.0}x{:.0} {}%{change}\n", object.id, object.color, object.x, object.y, object.left, object.top, object.width, object.height, object.share));
    }
    if let Some(previous) = previous {
        let gone: Vec<String> = previous.iter().filter(|old| !objects.iter().any(|object| object.id == old.id)).take(12).map(|old| format!("o{} {}", old.id, old.color)).collect();
        if !gone.is_empty() { text.push_str(&format!("- gone since the last look: {}\n", gone.join(", "))); }
    }
    text.push_str("### Regions (large areas)\n");
    for region in regions.iter().take(6) {
        let object = shape(region);
        text.push_str(&format!("- {} box {:.0},{:.0} {:.0}x{:.0} {}%\n", object.color, object.left, object.top, object.width, object.height, object.share));
    }
    Ok(Look { text, objects })
}
