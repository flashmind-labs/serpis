//! Mermaid diagrams in a chat, drawn as part of the page rather than shown as a picture.
//!
//! ariel-rs (a Rust port of Mermaid) lays the diagram out and writes it as SVG; this reads that
//! SVG back into boxes, lines, markers and text, and serpis paints them itself: in its own font,
//! in the theme's colors, at any zoom. What each shape is (a box, a group, a note, a line, an
//! edge's label) is told by the color ariel gave it, its default theme being a fixed palette.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::{Color32, Pos2, Rect, Stroke, Vec2, pos2, vec2};

/// What a shape's fill stands for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fill {
    None,
    /// A node's box.
    Node,
    /// A group (subgraph, composite state) behind its nodes.
    Group,
    Note,
    /// Behind an edge's label: the surface, so lines don't run through the text.
    Label,
    /// A white or near-white fill that isn't a node (alternate rows, an end state's ring).
    Plain,
    /// A dark fill (a start state, a fork bar, an arrowhead drawn as a shape).
    Solid,
}

/// What a shape's outline or a text's color stands for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Ink {
    Border,
    GroupBorder,
    NoteBorder,
    Line,
    Solid,
    Text,
    /// Labels on lines and group titles: quieter than a node's text.
    TextDim,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MarkerKind {
    Arrow,
    ArrowHollow,
    Diamond,
    DiamondHollow,
    Circle,
    Cross,
}

#[derive(Clone, Copy, Debug)]
pub struct Marker {
    pub kind: MarkerKind,
    /// Length along the line, in diagram units.
    pub len: f32,
    /// How far the tip lies beyond the path's end.
    pub tip: f32,
}

#[derive(Clone, Debug)]
pub enum Item {
    Rect { r: Rect, rx: f32, fill: Fill, stroke: Option<(Ink, f32)>, dash: bool, node: Option<usize> },
    Path { subpaths: Vec<(Vec<Pos2>, bool)>, fill: Fill, stroke: Option<(Ink, f32)>, dash: bool, start: Option<Marker>, end: Option<Marker>, edge: Option<(usize, usize)>, node: Option<usize> },
    Text { pos: Pos2, anchor: f32, baseline: Baseline, size: f32, bold: bool, lines: Vec<String>, ink: Ink, node: Option<usize> },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Baseline {
    Alphabetic,
    Middle,
    Hanging,
}

/// A diagram laid out, in ariel's units (its text at 16px).
#[derive(Debug, Default)]
pub struct Diagram {
    pub size: Vec2,
    pub items: Vec<Item>,
    /// Nodes by name, with their bounds (for hover, and for tests).
    pub nodes: Vec<(String, Rect)>,
    /// The kind of diagram, from its first word (flowchart, sequenceDiagram, ...).
    pub kind: String,
}

/// A diagram as a chat shows it: how much it is scaled, and its text size.
#[derive(Debug, Clone)]
pub struct Placed {
    pub diagram: Arc<Diagram>,
    /// Diagram units to points.
    pub scale: f32,
    /// Text size relative to `scale` (below 1 when serpis's font runs wider than ariel's).
    pub text: f32,
}

impl PartialEq for Placed {
    fn eq(&self, o: &Self) -> bool {
        Arc::ptr_eq(&self.diagram, &o.diagram) && self.scale == o.scale && self.text == o.text
    }
}

impl Placed {
    pub fn size(&self) -> Vec2 {
        self.diagram.size * self.scale
    }
}

/// The diagram `src` (Mermaid) laid out for a column `avail` points wide, its text the size of
/// a chat's (`font_px`, measured by `width_of`). A flowchart drawn left to right that doesn't
/// fit is drawn top to bottom instead, when that keeps it larger.
pub fn layout(src: &str, font_px: f32, width_of: &mut dyn FnMut(&str) -> f32, avail: f32) -> Result<Placed, String> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    src.hash(&mut h);
    font_px.to_bits().hash(&mut h);
    ((avail / 8.0).round() as i32).hash(&mut h);
    width_of("Mmmm abcdefghij 0123").to_bits().hash(&mut h);
    let key = h.finish();
    static CACHE: Mutex<Option<HashMap<u64, Result<Placed, String>>>> = Mutex::new(None);
    if let Some(hit) = CACHE.lock().unwrap().get_or_insert_with(HashMap::new).get(&key) {
        return hit.clone();
    }
    let placed = place(src, font_px, width_of, avail);
    let mut cache = CACHE.lock().unwrap();
    let cache = cache.get_or_insert_with(HashMap::new);
    if cache.len() > 256 {
        cache.clear();
    }
    cache.insert(key, placed.clone());
    placed
}

fn place(src: &str, font_px: f32, width_of: &mut dyn FnMut(&str) -> f32, avail: f32) -> Result<Placed, String> {
    let r = font_px / 16.0;
    let mut first = parse(&render(src)?)?;
    // The kind of diagram, as its source says (the SVG doesn't always).
    first.kind = src.lines().map(str::trim).find(|l| !l.is_empty() && !l.starts_with("%%")).and_then(|l| l.split_whitespace().next()).unwrap_or("").to_string();
    let fit = |d: &Diagram| (avail / (d.size.x * r)).min(1.0);
    let mut d = first;
    if fit(&d) < 1.0
        && let Some(turned) = turn_downward(src)
        && let Ok(mut t) = render(&turned).and_then(|s| parse(&s))
        && fit(&t) > fit(&d)
    {
        t.kind = std::mem::take(&mut d.kind);
        d = t;
    }
    let scale = r * fit(&d);
    // ariel sizes boxes for Liberation Sans; where serpis's font runs wider, its text is set a
    // little smaller so it stays inside them.
    let mut text = 1.0f32;
    let mut seen = 0;
    for item in &d.items {
        if let Item::Text { lines, size, .. } = item {
            for l in lines {
                if l.trim().is_empty() || seen > 60 {
                    continue;
                }
                seen += 1;
                let theirs = ariel_rs::backends::measure(l, *size as f64).0 as f32;
                let ours = width_of(l) * size / 16.0 / r;
                if ours > 0.0 && theirs > 0.0 {
                    text = text.min(theirs / ours);
                }
            }
        }
    }
    Ok(Placed { diagram: Arc::new(d), scale, text: text.clamp(0.8, 1.0) })
}

/// A left-to-right flowchart as top-to-bottom.
fn turn_downward(src: &str) -> Option<String> {
    let mut lines = src.lines().map(str::to_string).collect::<Vec<_>>();
    let head = lines.iter_mut().find(|l| !l.trim().is_empty() && !l.trim_start().starts_with("%%"))?;
    let words: Vec<&str> = head.split_whitespace().collect();
    if words.len() >= 2 && (words[0] == "flowchart" || words[0] == "graph") && matches!(words[1], "LR" | "RL") {
        *head = format!("{} TD", words[0]);
        return Some(lines.join("\n"));
    }
    None
}

/// ariel's SVG for `src`: an error when it can't draw it.
fn render(src: &str) -> Result<String, String> {
    // Init directives (themes, config) are for Mermaid's own renderer: serpis's theme decides.
    let src: String = src.lines().filter(|l| !l.trim_start().starts_with("%%{")).collect::<Vec<_>>().join("\n");
    match std::panic::catch_unwind(|| ariel_rs::try_render(&src, ariel_rs::theme::Theme::Default)) {
        Ok(Ok(svg)) => Ok(svg),
        Ok(Err(e)) => Err(e.to_string()),
        Err(_) => Err("the diagram's layout failed".into()),
    }
}

// ---------------------------------------------------------------------------------------------
// Reading the SVG

#[derive(Clone, Copy)]
struct Affine {
    sx: f32,
    sy: f32,
    tx: f32,
    ty: f32,
}

impl Affine {
    const ID: Affine = Affine { sx: 1.0, sy: 1.0, tx: 0.0, ty: 0.0 };
    fn apply(&self, p: Pos2) -> Pos2 {
        pos2(p.x * self.sx + self.tx, p.y * self.sy + self.ty)
    }
    fn then(&self, inner: Affine) -> Affine {
        Affine { sx: self.sx * inner.sx, sy: self.sy * inner.sy, tx: self.tx + inner.tx * self.sx, ty: self.ty + inner.ty * self.sy }
    }
}

fn transform(s: &str) -> Affine {
    let mut a = Affine::ID;
    let mut rest = s;
    while let Some(open) = rest.find('(') {
        let name = rest[..open].trim().trim_start_matches(',').trim();
        let Some(close) = rest[open..].find(')') else { break };
        let args: Vec<f32> = rest[open + 1..open + close].split(|c: char| c == ',' || c.is_whitespace()).filter(|s| !s.is_empty()).filter_map(|s| s.parse().ok()).collect();
        let t = match name {
            "translate" => Affine { tx: args.first().copied().unwrap_or(0.0), ty: args.get(1).copied().unwrap_or(0.0), ..Affine::ID },
            "scale" => {
                let sx = args.first().copied().unwrap_or(1.0);
                Affine { sx, sy: args.get(1).copied().unwrap_or(sx), ..Affine::ID }
            }
            "matrix" if args.len() == 6 => Affine { sx: args[0], sy: args[3], tx: args[4], ty: args[5] },
            _ => Affine::ID,
        };
        a = a.then(t);
        rest = &rest[open + close + 1..];
    }
    a
}

/// Presentation values, inherited: from attributes and `style`.
#[derive(Clone, Default)]
struct Style {
    fill: Option<String>,
    stroke: Option<String>,
    stroke_width: Option<f32>,
    dash: Option<String>,
    font_size: Option<f32>,
    font_weight: Option<String>,
    anchor: Option<String>,
    baseline: Option<String>,
    display_none: bool,
}

impl Style {
    fn read(&self, n: roxmltree::Node) -> Style {
        let mut s = self.clone();
        // A display or visibility of none only hides this element, so it's not inherited further
        // than the element itself; the caller checks it.
        s.display_none = false;
        let mut set = |k: &str, v: &str| {
            let v = v.trim();
            match k.trim() {
                "fill" => s.fill = Some(v.to_string()),
                "stroke" => s.stroke = Some(v.to_string()),
                "stroke-width" => s.stroke_width = v.trim_end_matches("px").parse().ok(),
                "stroke-dasharray" => s.dash = Some(v.to_string()),
                "font-size" => s.font_size = v.trim_end_matches("px").parse().ok(),
                "font-weight" => s.font_weight = Some(v.to_string()),
                "text-anchor" => s.anchor = Some(v.to_string()),
                "dominant-baseline" | "alignment-baseline" => s.baseline = Some(v.to_string()),
                "display" | "visibility" if v == "none" || v == "hidden" => s.display_none = true,
                _ => {}
            }
        };
        for a in n.attributes() {
            set(a.name(), a.value());
        }
        if let Some(style) = n.attribute("style") {
            for decl in style.split(';') {
                if let Some((k, v)) = decl.split_once(':') {
                    set(k, v);
                }
            }
        }
        s
    }
    fn dashed(&self) -> bool {
        self.dash.as_deref().is_some_and(|d| {
            let nums: Vec<f32> = d.split(|c: char| c == ',' || c.is_whitespace()).filter_map(|s| s.parse().ok()).collect();
            nums.len() >= 2 && nums.iter().skip(1).any(|g| *g > 0.0) || nums.len() == 1 && nums[0] > 0.0
        })
    }
}

fn color(s: &str) -> Option<[f32; 4]> {
    let s = s.trim().to_ascii_lowercase();
    let named = |n: &str| -> Option<[f32; 4]> {
        Some(match n {
            "white" => [1.0, 1.0, 1.0, 1.0],
            "black" => [0.0, 0.0, 0.0, 1.0],
            "none" | "transparent" => return None,
            "red" => [1.0, 0.0, 0.0, 1.0],
            "grey" | "gray" => [0.5, 0.5, 0.5, 1.0],
            "lightgrey" | "lightgray" => [0.83, 0.83, 0.83, 1.0],
            _ => [0.5, 0.5, 0.5, 1.0],
        })
    };
    if let Some(hex) = s.strip_prefix('#') {
        let v = |i: usize, n: usize| u8::from_str_radix(&hex[i..i + n], 16).ok().map(|x| if n == 1 { x * 17 } else { x } as f32 / 255.0);
        return match hex.len() {
            3 => Some([v(0, 1)?, v(1, 1)?, v(2, 1)?, 1.0]),
            6 => Some([v(0, 2)?, v(2, 2)?, v(4, 2)?, 1.0]),
            8 => Some([v(0, 2)?, v(2, 2)?, v(4, 2)?, v(6, 2)?]),
            _ => None,
        };
    }
    if let Some(inner) = s.strip_prefix("rgba(").or_else(|| s.strip_prefix("rgb(")) {
        let n: Vec<f32> = inner.trim_end_matches(')').split(',').filter_map(|x| x.trim().parse().ok()).collect();
        if n.len() >= 3 {
            return Some([n[0] / 255.0, n[1] / 255.0, n[2] / 255.0, n.get(3).copied().unwrap_or(1.0)]);
        }
        return None;
    }
    if s.starts_with("url(") {
        return None;
    }
    named(&s)
}

fn same(a: [f32; 4], hex: &str) -> bool {
    color(hex).is_some_and(|b| (0..3).all(|i| (a[i] - b[i]).abs() < 0.02))
}

fn light(c: [f32; 4]) -> f32 {
    0.2126 * c[0] + 0.7152 * c[1] + 0.0722 * c[2]
}

/// The role of a fill, from ariel's default palette.
fn fill_role(s: Option<&str>, ctx: &Ctx) -> Fill {
    let Some(c) = s.and_then(color) else { return Fill::None };
    if c[3] < 0.05 {
        return Fill::None;
    }
    if ctx.label {
        return Fill::Label;
    }
    if same(c, "#ECECFF") {
        return Fill::Node;
    }
    if same(c, "#fff5ad") {
        return Fill::Note;
    }
    if same(c, "#ffffde") {
        return if ctx.cluster { Fill::Group } else { Fill::Node };
    }
    if same(c, "#e8e8e8") {
        return Fill::Label;
    }
    if light(c) > 0.85 {
        return if ctx.cluster { Fill::Group } else { Fill::Plain };
    }
    if light(c) < 0.35 {
        return Fill::Solid;
    }
    Fill::Node
}

#[derive(Clone, Default)]
struct Ctx {
    affine: Option<Affine>,
    style: Style,
    cluster: bool,
    label: bool,
    node: Option<usize>,
}

struct Reader<'a> {
    out: Diagram,
    markers: HashMap<String, roxmltree::Node<'a, 'a>>,
    edge_ids: Vec<(usize, String)>,
}

fn parse(svg: &str) -> Result<Diagram, String> {
    let doc = roxmltree::Document::parse(svg).map_err(|e| format!("unreadable SVG: {e}"))?;
    let root = doc.root_element();
    let vb: Vec<f32> = root.attribute("viewBox").unwrap_or("0 0 0 0").split(|c: char| c == ',' || c.is_whitespace()).filter_map(|s| s.parse().ok()).collect();
    let (ox, oy, w, h) = match vb[..] {
        [x, y, w, h] => (x, y, w, h),
        _ => (0.0, 0.0, 0.0, 0.0),
    };
    let kind = root.attribute("aria-roledescription").unwrap_or("").to_string();
    let mut r = Reader { out: Diagram { size: vec2(w, h), kind, ..Default::default() }, markers: HashMap::new(), edge_ids: Vec::new() };
    for n in doc.descendants().filter(|n| n.has_tag_name("marker")) {
        if let Some(id) = n.attribute("id") {
            r.markers.insert(id.to_string(), n);
        }
    }
    let ctx = Ctx { affine: Some(Affine { tx: -ox, ty: -oy, ..Affine::ID }), ..Default::default() };
    r.walk(root, &ctx);
    // Lines named for the nodes they join (L_<from>_<to>_<n>): which nodes, for hover.
    let names: HashMap<String, usize> = r.out.nodes.iter().enumerate().map(|(i, (n, _))| (n.clone(), i)).collect();
    for (item, id) in std::mem::take(&mut r.edge_ids) {
        let Some(body) = id.rsplit_once("L_").map(|(_, b)| b) else { continue };
        let body = body.rsplit_once('_').map_or(body, |(b, _)| b);
        let ends = body.match_indices('_').find_map(|(at, _)| Some((*names.get(&body[..at])?, *names.get(&body[at + 1..])?)));
        if let (Some(ends), Some(Item::Path { edge, .. })) = (ends, r.out.items.get_mut(item)) {
            *edge = Some(ends);
        }
    }
    if r.out.size.x <= 0.0 || r.out.items.is_empty() {
        return Err("the diagram came out empty".into());
    }
    Ok(r.out)
}

impl<'a> Reader<'a> {
    fn walk(&mut self, n: roxmltree::Node<'a, 'a>, parent: &Ctx) {
        if !n.is_element() {
            return;
        }
        let tag = n.tag_name().name();
        if matches!(tag, "defs" | "marker" | "style" | "title" | "desc" | "metadata" | "foreignObject" | "clipPath" | "mask" | "linearGradient" | "radialGradient" | "pattern") {
            return;
        }
        let mut ctx = parent.clone();
        ctx.style = parent.style.read(n);
        if ctx.style.display_none {
            return;
        }
        if let Some(t) = n.attribute("transform") {
            ctx.affine = Some(parent.affine.unwrap_or(Affine::ID).then(transform(t)));
        }
        let class = n.attribute("class").unwrap_or("");
        let has = |c: &str| class.split_whitespace().any(|x| x == c);
        if has("cluster") {
            ctx.cluster = true;
        }
        if has("edgeLabel") || has("labelBkg") {
            ctx.label = true;
        }
        if tag == "g" && (has("node") || has("actor")) && ctx.node.is_none() {
            let id = n.attribute("id").unwrap_or("");
            ctx.node = Some(self.out.nodes.len());
            self.out.nodes.push((node_name(id), Rect::NOTHING));
        }
        let a = ctx.affine.unwrap_or(Affine::ID);
        let num = |k: &str| n.attribute(k).and_then(|v| v.trim_end_matches("px").parse::<f32>().ok()).unwrap_or(0.0);
        let st = &ctx.style;
        let stroke = |closed: bool, fill: Fill| -> Option<(Ink, f32)> {
            let c = color(st.stroke.as_deref()?)?;
            if c[3] < 0.05 {
                return None;
            }
            let w = st.stroke_width.unwrap_or(1.0);
            if w <= 0.0 {
                return None;
            }
            let ink = match fill {
                Fill::Note => Ink::NoteBorder,
                Fill::Group => Ink::GroupBorder,
                Fill::Solid => Ink::Solid,
                Fill::Label => return None,
                _ if !closed => Ink::Line,
                Fill::None if same(c, "#aaaa33") => Ink::GroupBorder,
                _ if same(c, "#333333") && fill == Fill::None => Ink::Line,
                _ => Ink::Border,
            };
            Some((ink, w))
        };
        let fill_of = |closed: bool| if closed { fill_role(st.fill.as_deref().or(Some("black")), &ctx) } else { Fill::None };
        let marker = |k: &str| -> Option<Marker> {
            let id = n.attribute(k)?;
            let id = id.trim().strip_prefix("url(#")?.trim_end_matches(')');
            self.markers.get(id).map(|m| read_marker(id, *m, k == "marker-start"))
        };
        match tag {
            "rect" => {
                let (x, y, w, h) = (num("x"), num("y"), num("width"), num("height"));
                if w > 0.0 && h > 0.0 {
                    let r = Rect::from_two_pos(a.apply(pos2(x, y)), a.apply(pos2(x + w, y + h)));
                    let fill = fill_of(true);
                    let item = Item::Rect { r, rx: num("rx").max(num("ry")) * a.sx, fill, stroke: stroke(true, fill), dash: st.dashed(), node: ctx.node };
                    self.push(item, ctx.node, r);
                }
            }
            "circle" | "ellipse" => {
                let (cx, cy) = (num("cx"), num("cy"));
                let (rx, ry) = if tag == "circle" { (num("r"), num("r")) } else { (num("rx"), num("ry")) };
                let pts: Vec<Pos2> = (0..48).map(|i| {
                    let t = i as f32 / 48.0 * std::f32::consts::TAU;
                    a.apply(pos2(cx + rx * t.cos(), cy + ry * t.sin()))
                }).collect();
                let fill = fill_of(true);
                let bounds = Rect::from_points(&pts);
                self.push(Item::Path { subpaths: vec![(pts, true)], fill, stroke: stroke(true, fill), dash: st.dashed(), start: None, end: None, edge: None, node: ctx.node }, ctx.node, bounds);
            }
            "line" => {
                let pts = vec![a.apply(pos2(num("x1"), num("y1"))), a.apply(pos2(num("x2"), num("y2")))];
                let bounds = Rect::from_points(&pts);
                let item = Item::Path { subpaths: vec![(pts, false)], fill: Fill::None, stroke: stroke(false, Fill::None).or(Some((Ink::Line, 1.0))), dash: st.dashed(), start: marker("marker-start"), end: marker("marker-end"), edge: None, node: ctx.node };
                self.push(item, ctx.node, bounds);
            }
            "polyline" | "polygon" => {
                let nums: Vec<f32> = n.attribute("points").unwrap_or("").split(|c: char| c == ',' || c.is_whitespace()).filter_map(|s| s.parse().ok()).collect();
                let pts: Vec<Pos2> = nums.chunks(2).filter(|c| c.len() == 2).map(|c| a.apply(pos2(c[0], c[1]))).collect();
                if pts.len() >= 2 {
                    let closed = tag == "polygon";
                    let fill = fill_of(closed);
                    let bounds = Rect::from_points(&pts);
                    self.push(Item::Path { subpaths: vec![(pts, closed)], fill, stroke: stroke(closed, fill), dash: st.dashed(), start: marker("marker-start"), end: marker("marker-end"), edge: None, node: ctx.node }, ctx.node, bounds);
                }
            }
            "path" => {
                let subpaths: Vec<(Vec<Pos2>, bool)> = flatten(n.attribute("d").unwrap_or("")).into_iter().map(|(p, c)| (p.into_iter().map(|q| a.apply(q)).collect(), c)).filter(|(p, _): &(Vec<Pos2>, bool)| p.len() >= 2).collect();
                if !subpaths.is_empty() {
                    let closed = subpaths.iter().all(|(_, c)| *c);
                    let fill = fill_of(closed && st.fill.as_deref() != Some("none"));
                    let bounds = Rect::from_points(&subpaths.iter().flat_map(|(p, _)| p.iter().copied()).collect::<Vec<_>>());
                    let at = self.out.items.len();
                    if let Some(id) = n.attribute("id").filter(|id| id.contains("L_")) {
                        self.edge_ids.push((at, id.to_string()));
                    }
                    self.push(Item::Path { subpaths, fill, stroke: stroke(closed, fill).or_else(|| (fill == Fill::None).then_some((Ink::Line, 1.0))), dash: st.dashed(), start: marker("marker-start"), end: marker("marker-end"), edge: None, node: ctx.node }, ctx.node, bounds);
                }
            }
            "text" => {
                self.text(n, &ctx, a);
                return;
            }
            _ => {}
        }
        for c in n.children() {
            self.walk(c, &ctx);
        }
    }

    fn push(&mut self, item: Item, node: Option<usize>, bounds: Rect) {
        if let Some(i) = node {
            let b = &mut self.out.nodes[i].1;
            *b = b.union(bounds);
        }
        self.out.items.push(item);
    }

    /// A `<text>`: its lines (tspans that move down a line, or `<br/>` written into it), where
    /// it sits and how it is aligned there.
    fn text(&mut self, n: roxmltree::Node, ctx: &Ctx, a: Affine) {
        let size = ctx.style.font_size.unwrap_or(16.0);
        let num = |n: roxmltree::Node, k: &str| -> Option<f32> {
            let v = n.attribute(k)?.trim();
            if let Some(em) = v.strip_suffix("em") { em.parse::<f32>().ok().map(|x| x * size) } else { v.trim_end_matches("px").parse().ok() }
        };
        let x0 = num(n, "x").unwrap_or(0.0);
        let mut y = num(n, "y").unwrap_or(0.0) + num(n, "dy").unwrap_or(0.0);
        let mut first_y = None;
        let mut lines: Vec<String> = Vec::new();
        let line_tspans: Vec<roxmltree::Node> = n.children().filter(|c| c.has_tag_name("tspan") && (c.attribute("dy").is_some() || c.attribute("y").is_some())).collect();
        if line_tspans.is_empty() {
            lines.push(n.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect::<String>());
            first_y = Some(y);
        } else {
            for t in line_tspans {
                if let Some(ty) = num(t, "y") {
                    y = ty;
                }
                y += num(t, "dy").unwrap_or(0.0);
                first_y.get_or_insert(y);
                let words: Vec<&str> = t.descendants().filter(|d| d.is_text()).filter_map(|d| d.text()).collect();
                lines.push(words.join(" ").split_whitespace().collect::<Vec<_>>().join(" "));
            }
        }
        // Labels written with <br/> come through as text, and quoted labels with their quotes.
        let lines: Vec<String> = lines.iter().flat_map(|l| l.split("<br/>").flat_map(|p| p.split("<br>")).map(|p| unquote(p.trim()).to_string()).collect::<Vec<_>>()).filter(|l| !l.is_empty()).collect();
        if lines.is_empty() {
            return;
        }
        let anchor = match ctx.style.anchor.as_deref() {
            Some("middle") => 0.5,
            Some("end") => 1.0,
            _ => 0.0,
        };
        let baseline = match ctx.style.baseline.as_deref() {
            Some("middle" | "central") => Baseline::Middle,
            Some("hanging" | "text-before-edge") => Baseline::Hanging,
            _ => Baseline::Alphabetic,
        };
        let pos = a.apply(pos2(x0, first_y.unwrap_or(y)));
        let bold = ctx.style.font_weight.as_deref().is_some_and(|w| w == "bold" || w.parse::<u32>().is_ok_and(|n| n >= 600));
        let ink = if ctx.label || ctx.cluster && ctx.node.is_none() { Ink::TextDim } else { Ink::Text };
        let est = Rect::from_center_size(pos, vec2(size * 0.55 * lines.iter().map(|l| l.chars().count()).max().unwrap_or(0) as f32, size * lines.len() as f32));
        self.push(Item::Text { pos, anchor, baseline, size: size * a.sy, bold, lines, ink, node: ctx.node }, ctx.node, est);
    }
}

/// A label written in quotes (`A["fn (x)"]`), without them.
fn unquote(s: &str) -> &str {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') { &s[1..s.len() - 1] } else { s }
}

/// A node's name from its element id: `mermaid-svg-flowchart-UI-0` is UI.
fn node_name(id: &str) -> String {
    let id = id.strip_prefix("mermaid-svg-").unwrap_or(id);
    let id = id.split_once("flowchart-").map_or(id, |(_, rest)| rest);
    match id.rsplit_once('-') {
        Some((name, n)) if n.chars().all(|c| c.is_ascii_digit()) => name.to_string(),
        _ => id.to_string(),
    }
}

fn read_marker(id: &str, m: roxmltree::Node, start: bool) -> Marker {
    let lower = id.to_ascii_lowercase();
    let kind = if lower.contains("cross") {
        MarkerKind::Cross
    } else if lower.contains("circle") || lower.contains("lollipop") {
        MarkerKind::Circle
    } else if lower.contains("composition") {
        MarkerKind::Diamond
    } else if lower.contains("aggregation") || lower.contains("diamond") {
        MarkerKind::DiamondHollow
    } else if lower.contains("extension") || lower.contains("open") {
        MarkerKind::ArrowHollow
    } else {
        MarkerKind::Arrow
    };
    let f = |k: &str| m.attribute(k).and_then(|v| v.parse::<f32>().ok());
    let vb: Vec<f32> = m.attribute("viewBox").unwrap_or("0 0 10 10").split(|c: char| c == ',' || c.is_whitespace()).filter_map(|s| s.parse().ok()).collect();
    let vbw = vb.get(2).copied().unwrap_or(10.0).max(0.1);
    let mw = f("markerWidth").unwrap_or(8.0);
    let ref_x = f("refX").unwrap_or(vbw / 2.0);
    let unit = mw / vbw;
    let tip = if start { ref_x * unit } else { (vbw - ref_x) * unit };
    Marker { kind, len: mw.clamp(6.0, 14.0), tip: tip.clamp(-mw, mw) }
}

/// An SVG path's subpaths as points (curves and arcs sampled), each with whether it closes.
fn flatten(d: &str) -> Vec<(Vec<Pos2>, bool)> {
    let mut toks: Vec<String> = Vec::new();
    let mut cur = String::new();
    let mut chars = d.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_alphabetic() && c != 'e' && c != 'E' {
            if !cur.is_empty() {
                toks.push(std::mem::take(&mut cur));
            }
            toks.push(c.to_string());
        } else if c == ',' || c.is_whitespace() {
            if !cur.is_empty() {
                toks.push(std::mem::take(&mut cur));
            }
        } else if c == '-' && !cur.is_empty() && !cur.ends_with('e') && !cur.ends_with('E') {
            toks.push(std::mem::take(&mut cur));
            cur.push(c);
        } else if c == '.' && cur.contains('.') && !cur.contains('e') {
            toks.push(std::mem::take(&mut cur));
            cur.push(c);
        } else {
            cur.push(c);
        }
    }
    if !cur.is_empty() {
        toks.push(cur);
    }
    let mut out: Vec<(Vec<Pos2>, bool)> = Vec::new();
    let mut pts: Vec<Pos2> = Vec::new();
    let (mut p, mut start) = (Pos2::ZERO, Pos2::ZERO);
    let mut last_ctrl: Option<Pos2> = None;
    let mut cmd = 'M';
    let mut i = 0;
    let num = |i: &mut usize| -> Option<f32> {
        let v = toks.get(*i)?.parse().ok()?;
        *i += 1;
        Some(v)
    };
    let finish = |pts: &mut Vec<Pos2>, closed: bool, out: &mut Vec<(Vec<Pos2>, bool)>| {
        if pts.len() >= 2 {
            out.push((std::mem::take(pts), closed));
        } else {
            pts.clear();
        }
    };
    while i < toks.len() {
        if let Some(c) = toks[i].chars().next().filter(|c| c.is_ascii_alphabetic()) {
            cmd = c;
            i += 1;
            if cmd == 'Z' || cmd == 'z' {
                p = start;
                finish(&mut pts, true, &mut out);
                last_ctrl = None;
                continue;
            }
        }
        let rel = cmd.is_ascii_lowercase();
        let base = if rel { p.to_vec2() } else { Vec2::ZERO };
        let before = i;
        match cmd.to_ascii_uppercase() {
            'M' => {
                let (Some(x), Some(y)) = (num(&mut i), num(&mut i)) else { break };
                finish(&mut pts, false, &mut out);
                p = pos2(x, y) + base;
                start = p;
                pts.push(p);
                cmd = if rel { 'l' } else { 'L' };
            }
            'L' => {
                let (Some(x), Some(y)) = (num(&mut i), num(&mut i)) else { break };
                p = pos2(x, y) + base;
                pts.push(p);
            }
            'H' => {
                let Some(x) = num(&mut i) else { break };
                p.x = x + base.x;
                pts.push(p);
            }
            'V' => {
                let Some(y) = num(&mut i) else { break };
                p.y = y + base.y;
                pts.push(p);
            }
            'C' | 'S' => {
                let c1 = if cmd.eq_ignore_ascii_case(&'C') {
                    let (Some(x), Some(y)) = (num(&mut i), num(&mut i)) else { break };
                    pos2(x, y) + base
                } else {
                    last_ctrl.map_or(p, |c| p + (p - c))
                };
                let (Some(x2), Some(y2), Some(x), Some(y)) = (num(&mut i), num(&mut i), num(&mut i), num(&mut i)) else { break };
                let (c2, e) = (pos2(x2, y2) + base, pos2(x, y) + base);
                for k in 1..=10 {
                    let t = k as f32 / 10.0;
                    let u = 1.0 - t;
                    pts.push(pos2(
                        u * u * u * p.x + 3.0 * u * u * t * c1.x + 3.0 * u * t * t * c2.x + t * t * t * e.x,
                        u * u * u * p.y + 3.0 * u * u * t * c1.y + 3.0 * u * t * t * c2.y + t * t * t * e.y,
                    ));
                }
                last_ctrl = Some(c2);
                p = e;
                if pts.is_empty() {
                    pts.push(p);
                }
                continue;
            }
            'Q' | 'T' => {
                let c = if cmd.eq_ignore_ascii_case(&'Q') {
                    let (Some(x), Some(y)) = (num(&mut i), num(&mut i)) else { break };
                    pos2(x, y) + base
                } else {
                    last_ctrl.map_or(p, |c| p + (p - c))
                };
                let (Some(x), Some(y)) = (num(&mut i), num(&mut i)) else { break };
                let e = pos2(x, y) + base;
                for k in 1..=8 {
                    let t = k as f32 / 8.0;
                    let u = 1.0 - t;
                    pts.push(pos2(u * u * p.x + 2.0 * u * t * c.x + t * t * e.x, u * u * p.y + 2.0 * u * t * c.y + t * t * e.y));
                }
                last_ctrl = Some(c);
                p = e;
                continue;
            }
            'A' => {
                let (Some(rx), Some(ry), Some(rot), Some(large), Some(sweep), Some(x), Some(y)) = (num(&mut i), num(&mut i), num(&mut i), num(&mut i), num(&mut i), num(&mut i), num(&mut i)) else { break };
                let e = pos2(x, y) + base;
                pts.extend(arc(p, e, rx.abs(), ry.abs(), rot, large != 0.0, sweep != 0.0));
                p = e;
            }
            _ => {
                i += 1;
            }
        }
        last_ctrl = None;
        if i == before {
            i += 1;
        }
    }
    finish(&mut pts, false, &mut out);
    out
}

/// An elliptical arc from `a` to `b`, sampled (SVG's endpoint form).
fn arc(a: Pos2, b: Pos2, mut rx: f32, mut ry: f32, rot_deg: f32, large: bool, sweep: bool) -> Vec<Pos2> {
    if rx < 1e-3 || ry < 1e-3 || a == b {
        return vec![b];
    }
    let phi = rot_deg.to_radians();
    let (s, c) = phi.sin_cos();
    let dx = (a.x - b.x) / 2.0;
    let dy = (a.y - b.y) / 2.0;
    let x1 = c * dx + s * dy;
    let y1 = -s * dx + c * dy;
    let lambda = x1 * x1 / (rx * rx) + y1 * y1 / (ry * ry);
    if lambda > 1.0 {
        rx *= lambda.sqrt();
        ry *= lambda.sqrt();
    }
    let num = (rx * rx * ry * ry - rx * rx * y1 * y1 - ry * ry * x1 * x1).max(0.0);
    let den = rx * rx * y1 * y1 + ry * ry * x1 * x1;
    let mut k = (num / den.max(1e-9)).sqrt();
    if large == sweep {
        k = -k;
    }
    let cx1 = k * rx * y1 / ry;
    let cy1 = -k * ry * x1 / rx;
    let cx = c * cx1 - s * cy1 + (a.x + b.x) / 2.0;
    let cy = s * cx1 + c * cy1 + (a.y + b.y) / 2.0;
    let ang = |ux: f32, uy: f32, vx: f32, vy: f32| (ux * vy - uy * vx).atan2(ux * vx + uy * vy);
    let t1 = ang(1.0, 0.0, (x1 - cx1) / rx, (y1 - cy1) / ry);
    let mut dt = ang((x1 - cx1) / rx, (y1 - cy1) / ry, (-x1 - cx1) / rx, (-y1 - cy1) / ry);
    if !sweep && dt > 0.0 {
        dt -= std::f32::consts::TAU;
    } else if sweep && dt < 0.0 {
        dt += std::f32::consts::TAU;
    }
    let n = ((dt.abs() / std::f32::consts::TAU) * 32.0).ceil().max(2.0) as usize;
    (1..=n).map(|i| {
        let t = t1 + dt * i as f32 / n as f32;
        let (st, ct) = t.sin_cos();
        pos2(cx + rx * ct * c - ry * st * s, cy + rx * ct * s + ry * st * c)
    }).collect()
}

// ---------------------------------------------------------------------------------------------
// Painting

/// Paint `p` with its top left at `origin`, on `surface` (what's behind it). Returns the node
/// under `hover`, if any; it and its lines are drawn in the accent color.
pub fn paint(painter: &egui::Painter, origin: Pos2, p: &Placed, theme: &crate::theme::Theme, surface: Color32, hover: Option<Pos2>) -> Option<usize> {
    use crate::themes::mix;
    let d = &p.diagram;
    let s = p.scale;
    let at = |q: Pos2| origin + q.to_vec2() * s;
    let k = theme.ui_scale();
    let hovered = hover.and_then(|h| {
        d.nodes.iter().enumerate().filter(|(_, (_, r))| r.is_positive()).find(|(_, (_, r))| Rect::from_min_max(at(r.min), at(r.max)).contains(h)).map(|(i, _)| i)
    });
    let fg = theme.fg;
    let fill_color = |f: Fill| match f {
        Fill::None => Color32::TRANSPARENT,
        Fill::Node => mix(surface, fg, if theme.dark { 0.075 } else { 0.045 }),
        Fill::Group => mix(surface, fg, if theme.dark { 0.03 } else { 0.022 }),
        Fill::Note => mix(mix(surface, fg, if theme.dark { 0.04 } else { 0.0 }), theme.yellow, if theme.dark { 0.07 } else { 0.1 }),
        Fill::Label => surface,
        Fill::Plain => mix(surface, fg, if theme.dark { 0.035 } else { 0.015 }),
        Fill::Solid => mix(surface, fg, 0.72),
    };
    let ink_color = |i: Ink| match i {
        Ink::Border => mix(surface, fg, if theme.dark { 0.24 } else { 0.2 }),
        Ink::GroupBorder => mix(surface, fg, 0.12),
        Ink::NoteBorder => mix(surface, theme.yellow, 0.45),
        Ink::Line => mix(surface, fg, 0.48),
        Ink::Solid => mix(surface, fg, 0.72),
        Ink::Text => fg,
        Ink::TextDim => theme.fg_dim,
    };
    let lit = |node: Option<usize>, edge: Option<(usize, usize)>| hovered.is_some() && (node == hovered || edge.is_some_and(|(a, b)| Some(a) == hovered || Some(b) == hovered));
    let family = theme.format(crate::theme::Face::AssistantText).font_id.family;
    let radius = (theme.radius as f32).clamp(4.0, 8.0) * k;
    // Shapes, then text over them: a label is never under a line drawn after it.
    let shapes = d.items.iter().filter(|i| !matches!(i, Item::Text { .. }));
    let texts = d.items.iter().filter(|i| matches!(i, Item::Text { .. }));
    for item in shapes.chain(texts) {
        match item {
            Item::Rect { r, rx, fill, stroke, dash, node } => {
                let rect = Rect::from_min_max(at(r.min), at(r.max));
                // Boxes, groups and notes take serpis's corner radius; other rects keep theirs.
                let round = match fill {
                    Fill::Node | Fill::Group | Fill::Note | Fill::Plain => radius.min(rect.height() / 2.0),
                    Fill::Label => 3.0 * k,
                    _ => rx * s,
                };
                let on = lit(*node, None);
                let st = stroke.map(|(i, w)| Stroke::new(stroke_w(i, w, k), if on && i == Ink::Border { theme.accent } else { ink_color(i) }));
                if *dash && st.is_some() {
                    painter.rect_filled(rect, round, fill_color(*fill));
                    let pts = vec![rect.left_top(), rect.right_top(), rect.right_bottom(), rect.left_bottom(), rect.left_top()];
                    painter.extend(egui::Shape::dashed_line(&pts, st.unwrap(), 4.0 * k, 3.0 * k));
                } else {
                    painter.rect(rect, round, fill_color(*fill), st.unwrap_or(Stroke::NONE), egui::StrokeKind::Middle);
                }
            }
            Item::Path { subpaths, fill, stroke, dash, start, end, edge, node } => {
                let on = lit(*node, *edge);
                let line = stroke.map(|(i, w)| {
                    let c = if on && matches!(i, Ink::Line | Ink::Border) { theme.accent } else { ink_color(i) };
                    Stroke::new(stroke_w(i, w, k) * if on && edge.is_some() { 1.4 } else { 1.0 }, c)
                });
                for (pts, closed) in subpaths {
                    let pts: Vec<Pos2> = pts.iter().map(|q| at(*q)).collect();
                    if *closed && *fill != Fill::None {
                        painter.add(egui::Shape::convex_polygon(pts.clone(), fill_color(*fill), Stroke::NONE));
                    }
                    if let Some(st) = line {
                        let mut drawn = pts.clone();
                        if *closed {
                            drawn.push(pts[0]);
                        }
                        if *dash {
                            painter.extend(egui::Shape::dashed_line(&drawn, st, 4.0 * k, 3.5 * k));
                        } else {
                            painter.add(egui::Shape::line(drawn, st));
                        }
                    }
                }
                let color = line.map_or(ink_color(Ink::Line), |l| l.color);
                if let Some((pts, _)) = subpaths.first()
                    && let Some(m) = start
                {
                    let pts: Vec<Pos2> = pts.iter().map(|q| at(*q)).collect();
                    marker(painter, &pts, true, m, s, color, surface);
                }
                if let Some((pts, _)) = subpaths.last()
                    && let Some(m) = end
                {
                    let pts: Vec<Pos2> = pts.iter().map(|q| at(*q)).collect();
                    marker(painter, &pts, false, m, s, color, surface);
                }
            }
            Item::Text { pos, anchor, baseline, size, lines, ink, node, .. } => {
                let px = size * s * p.text;
                let font = egui::FontId::new(px, family.clone());
                let color = if lit(*node, None) && *ink == Ink::Text { theme.accent } else { ink_color(*ink) };
                let galleys: Vec<_> = lines.iter().map(|l| painter.layout_no_wrap(l.clone(), font.clone(), color)).collect();
                let row = px * 1.25;
                let total = row * galleys.len() as f32;
                let p0 = at(*pos);
                // Where the first line's top goes: SVG places text by its baseline, or its middle.
                let top = match baseline {
                    Baseline::Alphabetic => p0.y - px * 0.8 - (row - px) / 2.0,
                    Baseline::Middle => p0.y - total / 2.0,
                    Baseline::Hanging => p0.y,
                };
                let placed: Vec<(Pos2, Arc<egui::Galley>)> = galleys.into_iter().enumerate().map(|(j, g)| {
                    let y = top + j as f32 * row + (row - g.size().y) / 2.0;
                    (pos2(p0.x - g.size().x * anchor, y), g)
                }).collect();
                // Text on its own (a message's, a block's) gets the surface behind it, as a line's
                // label does, so the lines it crosses (a lifeline, a self-message) don't run
                // through its letters.
                if *ink == Ink::Text && node.is_none() {
                    for (at, g) in &placed {
                        painter.rect_filled(Rect::from_min_size(*at, g.size()).expand2(vec2(3.0 * k, 0.0)), 3.0 * k, surface);
                    }
                }
                for (at, g) in placed {
                    painter.galley(at, g, color);
                }
            }
        }
    }
    hovered
}

fn stroke_w(i: Ink, w: f32, k: f32) -> f32 {
    match i {
        Ink::Line => (if w >= 1.8 { 2.0 } else { 1.2 }) * k,
        Ink::Solid => w.min(2.0) * k,
        _ => 1.0 * k,
    }
}

fn marker(painter: &egui::Painter, pts: &[Pos2], start: bool, m: &Marker, s: f32, color: Color32, surface: Color32) {
    let (end, prev) = if start {
        (pts[0], pts.iter().skip(1).find(|q| q.distance(pts[0]) > 0.5).copied())
    } else {
        let e = *pts.last().unwrap();
        (e, pts.iter().rev().skip(1).find(|q| q.distance(e) > 0.5).copied())
    };
    let Some(prev) = prev else { return };
    let u = (end - prev).normalized();
    let n = vec2(-u.y, u.x);
    let tip = end + u * (m.tip * s).max(0.0);
    let len = (m.len * s).clamp(6.0, 11.0);
    let half = len * 0.42;
    let base = tip - u * len;
    match m.kind {
        MarkerKind::Arrow => {
            painter.add(egui::Shape::convex_polygon(vec![tip, base + n * half, base - n * half], color, Stroke::NONE));
        }
        MarkerKind::ArrowHollow => {
            painter.add(egui::Shape::convex_polygon(vec![tip, base + n * half, base - n * half], surface, Stroke::new(1.0, color)));
        }
        MarkerKind::Diamond | MarkerKind::DiamondHollow => {
            let mid = tip - u * len / 2.0;
            let pts = vec![tip, mid + n * half * 0.8, tip - u * len, mid - n * half * 0.8];
            let fill = if m.kind == MarkerKind::Diamond { color } else { surface };
            painter.add(egui::Shape::convex_polygon(pts, fill, Stroke::new(1.0, color)));
        }
        MarkerKind::Circle => {
            painter.circle(tip - u * half, half * 0.8, color, Stroke::NONE);
        }
        MarkerKind::Cross => {
            let c = tip - u * half;
            let a = (u + n) * half * 0.7;
            let b = (u - n) * half * 0.7;
            painter.line_segment([c - a, c + a], Stroke::new(1.4, color));
            painter.line_segment([c - b, c + b], Stroke::new(1.4, color));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLOW: &str = "flowchart LR\n  subgraph Editor[\"serpis (editor)\"]\n    UI[egui UI] --> Chat[Chat buffer]\n  end\n  Chat -- prompt --> Loop{Run loop}\n  Loop -. events .-> Chat";

    fn chars(s: &str) -> f32 {
        s.chars().count() as f32 * 7.5
    }

    #[test]
    fn a_flowchart_is_read_into_boxes_lines_and_text() {
        let p = layout(FLOW, 14.0, &mut chars, 2000.0).unwrap();
        let d = &p.diagram;
        let names: Vec<&str> = d.nodes.iter().map(|(n, _)| n.as_str()).collect();
        for want in ["UI", "Chat", "Loop"] {
            assert!(names.contains(&want), "{names:?}");
        }
        let texts: Vec<String> = d.items.iter().filter_map(|i| if let Item::Text { lines, .. } = i { Some(lines.join("/")) } else { None }).collect();
        assert!(texts.iter().any(|t| t == "serpis (editor)"), "the group's title, without its quotes: {texts:?}");
        assert!(texts.iter().any(|t| t == "prompt"), "{texts:?}");
        assert!(d.items.iter().any(|i| matches!(i, Item::Rect { fill: Fill::Group, .. })), "the group has its surface");
        assert!(d.items.iter().any(|i| matches!(i, Item::Path { edge: Some(_), end: Some(_), .. })), "lines know their ends and have arrows");
        assert!(d.items.iter().any(|i| matches!(i, Item::Path { dash: true, .. })), "the dotted line stays dotted");
        assert!((p.scale - 14.0 / 16.0).abs() < 1e-3, "fits, so text at the chat's size");
    }

    #[test]
    fn a_wide_flowchart_turns_downward_to_fit() {
        let wide = layout(FLOW, 14.0, &mut chars, 2000.0).unwrap();
        let narrow = layout(FLOW, 14.0, &mut chars, 300.0).unwrap();
        assert!(narrow.diagram.size.y > narrow.diagram.size.x, "drawn top to bottom: {:?}", narrow.diagram.size);
        assert!(narrow.scale > wide.scale * (300.0 / (wide.diagram.size.x * wide.scale)), "and kept larger than shrinking it would");
    }

    #[test]
    fn sequence_and_state_diagrams_are_drawn() {
        for src in ["sequenceDiagram\n  actor U as User\n  U->>+C: Send\n  C-->>-U: done", "stateDiagram-v2\n  [*] --> Idle\n  Idle --> Writing: press c\n  Writing --> [*]"] {
            let p = layout(src, 14.0, &mut chars, 800.0).unwrap();
            assert!(p.diagram.items.len() > 4, "{src}");
        }
    }

    #[test]
    fn what_it_cant_draw_is_an_error() {
        assert!(layout("not a diagram at all", 14.0, &mut chars, 800.0).is_err());
    }

    #[test]
    fn paths_flatten() {
        let p = flatten("M0,0 L10,0 Q15,0 15,5 L15,20 A5,5 0 0,1 10,25 Z");
        assert_eq!(p.len(), 1);
        assert!(p[0].1, "closed");
        let last = *p[0].0.last().unwrap();
        assert!((last.x - 10.0).abs() < 0.1 && (last.y - 25.0).abs() < 0.1, "{last:?}");
        assert_eq!(flatten("M1-2l3-4").first().map(|(p, _)| p.len()), Some(2));
    }
}
