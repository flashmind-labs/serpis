//! Window layout, i3-style.
//!
//! A workspace's layout is a tree: leaves are windows (views onto buffers) and inner nodes
//! are containers holding any number of children side by side (`splith`), stacked
//! (`splitv`), as tabs (`tabbed`) or as a stack of title bars (`stacked`). Containers have
//! stable ids, so a whole container can be selected (`focus-parent`) and moved like a window.
//!
//! Every operation keeps the tree normalized: no empty containers, no single-child
//! containers below the root, and child sizes that sum to 1.

use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::buffer::WindowId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    SplitH,
    SplitV,
    Tabbed,
    Stacked,
}

impl Kind {
    /// Whether children are laid out (or navigated) left to right.
    pub fn horizontal(self) -> bool {
        matches!(self, Kind::SplitH | Kind::Tabbed)
    }
    /// Whether only the focused child is shown.
    pub fn single(self) -> bool {
        matches!(self, Kind::Tabbed | Kind::Stacked)
    }
    pub fn name(self) -> &'static str {
        match self {
            Kind::SplitH => "splith",
            Kind::SplitV => "splitv",
            Kind::Tabbed => "tabbed",
            Kind::Stacked => "stacked",
        }
    }
    pub fn parse(s: &str) -> Option<Kind> {
        Some(match s {
            "splith" | "right" | "horizontal" => Kind::SplitH,
            "splitv" | "below" | "vertical" => Kind::SplitV,
            "tabbed" | "tabs" => Kind::Tabbed,
            "stacked" | "stacking" => Kind::Stacked,
            _ => return None,
        })
    }
}

/// A direction for focus, move and resize.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    Left,
    Right,
    Up,
    Down,
}

impl Dir {
    pub fn parse(s: &str) -> Option<Dir> {
        Some(match s {
            "left" => Dir::Left,
            "right" => Dir::Right,
            "up" => Dir::Up,
            "down" => Dir::Down,
            _ => return None,
        })
    }
    pub fn horizontal(self) -> bool {
        matches!(self, Dir::Left | Dir::Right)
    }
    pub fn forward(self) -> bool {
        matches!(self, Dir::Right | Dir::Down)
    }
}

/// Where a dropped node lands relative to a target window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zone {
    /// Split the target, putting the node on that side.
    Edge(Dir),
    /// Group with the target as tabs.
    Center,
}

impl Zone {
    pub fn parse(s: &str) -> Option<Zone> {
        match s {
            "center" | "tab" => Some(Zone::Center),
            d => Dir::parse(d).map(Zone::Edge),
        }
    }
}

#[derive(Debug, Clone)]
pub enum Node {
    Window(WindowId),
    Container(Container),
}

#[derive(Debug, Clone)]
pub struct Container {
    pub id: u64,
    pub kind: Kind,
    pub children: Vec<Node>,
    /// Fraction of the container each child takes (split kinds).
    pub sizes: Vec<f32>,
    /// The child that last had focus: shown in tabbed/stacked, entered by directional focus.
    pub focus: usize,
}

/// A window or a container, by identity (stable across tree edits).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRef {
    Window(WindowId),
    Container(u64),
}

static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn next_id() -> u64 {
    NEXT_ID.fetch_add(1, Ordering::Relaxed)
}

impl Node {
    pub fn as_ref(&self) -> NodeRef {
        match self {
            Node::Window(w) => NodeRef::Window(*w),
            Node::Container(c) => NodeRef::Container(c.id),
        }
    }
    fn is(&self, r: NodeRef) -> bool {
        self.as_ref() == r
    }
    /// Windows in this subtree, in order.
    pub fn leaves(&self, out: &mut Vec<WindowId>) {
        match self {
            Node::Window(w) => out.push(*w),
            Node::Container(c) => c.children.iter().for_each(|n| n.leaves(out)),
        }
    }
    /// The window focus lands on when entering this subtree.
    pub fn focus_leaf(&self) -> Option<WindowId> {
        match self {
            Node::Window(w) => Some(*w),
            Node::Container(c) => c.children.get(c.focus).or(c.children.first())?.focus_leaf(),
        }
    }
}

impl Container {
    pub fn new(kind: Kind, children: Vec<Node>) -> Self {
        let n = children.len().max(1);
        Container {
            id: next_id(),
            kind,
            sizes: vec![1.0 / n as f32; children.len()],
            children,
            focus: 0,
        }
    }

    fn insert(&mut self, i: usize, node: Node) {
        let i = i.min(self.children.len());
        // The newcomer gets an equal share; the others shrink proportionally.
        let n = self.children.len() as f32 + 1.0;
        for s in &mut self.sizes {
            *s *= (n - 1.0) / n;
        }
        self.sizes.insert(i, 1.0 / n);
        self.children.insert(i, node);
        if self.focus >= i && self.children.len() > 1 {
            self.focus += 1;
        }
    }

    fn take(&mut self, i: usize) -> Node {
        self.sizes.remove(i);
        let total: f32 = self.sizes.iter().sum();
        if total > 0.0 {
            for s in &mut self.sizes {
                *s /= total;
            }
        }
        let node = self.children.remove(i);
        if self.focus > i {
            self.focus -= 1;
        }
        self.focus = self.focus.min(self.children.len().saturating_sub(1));
        node
    }
}

/// One workspace's tree. The root is always a container.
#[derive(Debug, Clone)]
pub struct LayoutTree {
    pub root: Container,
    /// A window shown alone over the whole workspace (i3 `fullscreen`).
    pub fullscreen: Option<WindowId>,
}

impl LayoutTree {
    pub fn new(w: WindowId) -> Self {
        LayoutTree {
            root: Container::new(Kind::SplitH, vec![Node::Window(w)]),
            fullscreen: None,
        }
    }

    pub fn leaves(&self) -> Vec<WindowId> {
        let mut out = Vec::new();
        self.root.children.iter().for_each(|n| n.leaves(&mut out));
        out
    }

    pub fn contains(&self, w: WindowId) -> bool {
        self.path(NodeRef::Window(w)).is_some()
    }

    /// Child indices from the root to `r` (empty for the root itself).
    pub fn path(&self, r: NodeRef) -> Option<Vec<usize>> {
        fn walk(c: &Container, r: NodeRef, path: &mut Vec<usize>) -> bool {
            if NodeRef::Container(c.id) == r {
                return true;
            }
            for (i, n) in c.children.iter().enumerate() {
                path.push(i);
                match n {
                    Node::Window(_) if n.is(r) => return true,
                    Node::Container(cc) if walk(cc, r, path) => return true,
                    _ => {}
                }
                path.pop();
            }
            false
        }
        let mut path = Vec::new();
        walk(&self.root, r, &mut path).then_some(path)
    }

    pub fn container(&self, path: &[usize]) -> Option<&Container> {
        let mut c = &self.root;
        for &i in path {
            match c.children.get(i)? {
                Node::Container(cc) => c = cc,
                Node::Window(_) => return None,
            }
        }
        Some(c)
    }

    pub fn container_mut(&mut self, path: &[usize]) -> Option<&mut Container> {
        let mut c = &mut self.root;
        for &i in path {
            match c.children.get_mut(i)? {
                Node::Container(cc) => c = cc,
                Node::Window(_) => return None,
            }
        }
        Some(c)
    }

    pub fn node(&self, r: NodeRef) -> Option<&Node> {
        let path = self.path(r)?;
        let (last, parent) = path.split_last()?;
        self.container(parent)?.children.get(*last)
    }

    /// The container directly holding `r` (the root holds itself).
    pub fn parent_of(&self, r: NodeRef) -> Option<u64> {
        let path = self.path(r)?;
        if path.is_empty() {
            return Some(self.root.id);
        }
        self.container(&path[..path.len() - 1]).map(|c| c.id)
    }

    /// Record `w` as the focused leaf all the way up, so tabs and directional focus follow.
    pub fn note_focus(&mut self, w: WindowId) {
        let Some(path) = self.path(NodeRef::Window(w)) else { return };
        let mut c = &mut self.root;
        for &i in &path {
            c.focus = i;
            match &mut c.children[i] {
                Node::Container(cc) => c = cc,
                Node::Window(_) => break,
            }
        }
    }

    /// Windows that are on screen: everything except unfocused tabs/stack entries, or just
    /// the fullscreen window.
    pub fn visible_leaves(&self) -> Vec<WindowId> {
        if let Some(w) = self.fullscreen.filter(|w| self.contains(*w)) {
            return vec![w];
        }
        fn walk(c: &Container, out: &mut Vec<WindowId>) {
            let shown: Vec<&Node> = if c.kind.single() { c.children.get(c.focus).into_iter().collect() } else { c.children.iter().collect() };
            for n in shown {
                match n {
                    Node::Window(w) => out.push(*w),
                    Node::Container(cc) => walk(cc, out),
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.root, &mut out);
        out
    }

    /// Remove empty containers, splice single-child containers into their parent, and keep
    /// sizes summing to 1. A root left holding one container adopts it.
    pub fn normalize(&mut self) {
        fn fix(c: &mut Container) {
            let mut i = 0;
            while i < c.children.len() {
                if let Node::Container(cc) = &mut c.children[i] {
                    fix(cc);
                    match cc.children.len() {
                        0 => {
                            c.take(i);
                            continue;
                        }
                        1 => {
                            let only = cc.children.pop().unwrap();
                            c.children[i] = only;
                        }
                        _ => {}
                    }
                }
                i += 1;
            }
            if c.sizes.len() != c.children.len() {
                c.sizes = vec![1.0 / c.children.len().max(1) as f32; c.children.len()];
            }
            let total: f32 = c.sizes.iter().sum();
            if total > 0.0 && (total - 1.0).abs() > 1e-4 {
                c.sizes.iter_mut().for_each(|s| *s /= total);
            }
            c.focus = c.focus.min(c.children.len().saturating_sub(1));
        }
        fix(&mut self.root);
        while self.root.children.len() == 1 {
            match &self.root.children[0] {
                Node::Container(_) => {
                    let Node::Container(only) = self.root.children.pop().unwrap() else { unreachable!() };
                    self.root = only;
                }
                Node::Window(_) => break,
            }
        }
        if self.fullscreen.is_some_and(|w| !self.contains(w)) {
            self.fullscreen = None;
        }
    }

    /// Detach `r` from the tree. The root can't be removed.
    pub fn remove(&mut self, r: NodeRef) -> Option<Node> {
        let path = self.path(r)?;
        let (last, parent) = path.split_last()?;
        let node = self.container_mut(parent)?.take(*last);
        self.normalize();
        Some(node)
    }

    /// Insert `node` into container `cid` at `index`.
    pub fn insert(&mut self, cid: u64, index: usize, node: Node) -> bool {
        let Some(path) = self.path(NodeRef::Container(cid)) else { return false };
        let Some(c) = self.container_mut(&path) else { return false };
        c.insert(index, node);
        self.normalize();
        true
    }

    /// Insert `node` next to `anchor` (after it), in the anchor's container.
    pub fn insert_beside(&mut self, anchor: NodeRef, node: Node) -> bool {
        let Some(path) = self.path(anchor) else { return false };
        let Some((last, parent)) = path.split_last() else {
            self.root.insert(self.root.children.len(), node);
            return true;
        };
        let Some(c) = self.container_mut(parent) else { return false };
        c.insert(*last + 1, node);
        self.normalize();
        true
    }

    /// Open `new` next to `target`. A container already laid out in the requested
    /// orientation (or holding only `target`) gets a new child; otherwise `target` is wrapped
    /// in a new container of that orientation.
    pub fn split(&mut self, target: WindowId, new: WindowId, kind: Kind) {
        let Some(path) = self.path(NodeRef::Window(target)) else { return };
        let Some((last, parent)) = path.split_last() else { return };
        let Some(c) = self.container_mut(parent) else { return };
        if c.children.len() == 1 && !c.kind.single() {
            c.kind = kind;
        }
        if c.kind == kind {
            // The new window takes half of the target's share.
            let half = c.sizes[*last] / 2.0;
            c.children.insert(*last + 1, Node::Window(new));
            c.sizes[*last] = half;
            c.sizes.insert(*last + 1, half);
        } else {
            let mut wrap = Container::new(kind, vec![Node::Window(target), Node::Window(new)]);
            wrap.focus = 1;
            c.children[*last] = Node::Container(wrap);
        }
        self.normalize();
        self.note_focus(new);
    }

    /// Whether `inner` is `outer` or lies inside it.
    pub fn within(&self, inner: NodeRef, outer: NodeRef) -> bool {
        match (self.path(inner), self.path(outer)) {
            (Some(i), Some(o)) => i.starts_with(&o),
            _ => false,
        }
    }

    /// Drop `r` on window/container `target`: beside it on an edge (wrapping the target when
    /// its container runs the other way), or into a tab group with it at the center.
    pub fn place(&mut self, r: NodeRef, target: NodeRef, zone: Zone) -> bool {
        if self.within(target, r) || self.path(target).is_none_or(|p| p.is_empty()) {
            return false;
        }
        let Some(node) = self.remove(r) else { return false };
        let Some(path) = self.path(target) else { return false };
        let (idx, parent) = match path.split_last() {
            Some((i, p)) => (*i, p.to_vec()),
            // The target became the root (removing `r` collapsed everything into it).
            None => {
                let kind = match zone {
                    Zone::Edge(d) if d.horizontal() => Kind::SplitH,
                    Zone::Edge(_) => Kind::SplitV,
                    Zone::Center => Kind::Tabbed,
                };
                let first = matches!(zone, Zone::Edge(Dir::Left | Dir::Up));
                let old = std::mem::replace(&mut self.root, Container::new(kind, Vec::new()));
                let mut kids = vec![Node::Container(old), node];
                if first {
                    kids.reverse();
                }
                self.root = Container::new(kind, kids);
                self.root.focus = if first { 0 } else { 1 };
                self.normalize();
                return true;
            }
        };
        let c = self.container_mut(&parent).unwrap();
        let (kind, fwd) = match zone {
            Zone::Edge(d) => (if d.horizontal() { Kind::SplitH } else { Kind::SplitV }, d.forward()),
            Zone::Center => (Kind::Tabbed, true),
        };
        let joins = match zone {
            Zone::Edge(_) => c.kind == kind,
            Zone::Center => c.kind.single(),
        };
        if joins {
            let at = if fwd { idx + 1 } else { idx };
            c.insert(at, node);
            c.focus = at;
        } else {
            let t = std::mem::replace(&mut c.children[idx], Node::Window(WindowId::default()));
            let mut kids = vec![t, node];
            if !fwd {
                kids.reverse();
            }
            let mut wrap = Container::new(kind, kids);
            wrap.focus = if fwd { 1 } else { 0 };
            c.children[idx] = Node::Container(wrap);
            c.focus = idx;
        }
        self.normalize();
        true
    }

    /// Put `r` at `index` among container `cid`'s children (tab bars: insert or reorder).
    pub fn place_in(&mut self, r: NodeRef, cid: u64, index: usize) -> bool {
        let target = NodeRef::Container(cid);
        if self.within(target, r) {
            return false;
        }
        if self.parent_of(r) == Some(cid) {
            // Reorder in place: removing first could collapse a two-child group.
            let path = self.path(target).unwrap();
            let c = self.container_mut(&path).unwrap();
            let Some(from) = c.children.iter().position(|n| n.is(r)) else { return false };
            let to = if index > from { index - 1 } else { index }.min(c.children.len() - 1);
            let n = c.children.remove(from);
            let sz = c.sizes.remove(from);
            c.children.insert(to, n);
            c.sizes.insert(to, sz);
            c.focus = to;
            return true;
        }
        let Some(node) = self.remove(r) else { return false };
        let Some(path) = self.path(target) else {
            // Shouldn't happen (cid didn't hold r); put the node back at the root.
            self.root.insert(self.root.children.len(), node);
            self.normalize();
            return false;
        };
        let c = self.container_mut(&path).unwrap();
        let at = index.min(c.children.len());
        c.insert(at, node);
        c.focus = at;
        self.normalize();
        true
    }

    /// Directional focus, i3-style: climb to the nearest container laid out along `dir` that
    /// has a neighbor that way, then enter the neighbor at its last-focused window.
    pub fn focus_dir(&self, from: WindowId, dir: Dir) -> Option<WindowId> {
        let path = self.path(NodeRef::Window(from))?;
        for depth in (0..path.len()).rev() {
            let c = self.container(&path[..depth])?;
            if c.kind.horizontal() != dir.horizontal() {
                continue;
            }
            let i = path[depth];
            let j = if dir.forward() { i + 1 } else { i.checked_sub(1)? };
            if let Some(n) = c.children.get(j) {
                return n.focus_leaf();
            }
        }
        None
    }

    /// Move `r` one step in `dir`, i3-style:
    /// - next to a window in its container: swap with it;
    /// - next to a container: move into it;
    /// - at the edge of its container: move out, beside the container, into the nearest
    ///   ancestor laid out along `dir`;
    /// - at the edge of the workspace: the root is re-laid out along `dir` around it.
    pub fn move_node(&mut self, r: NodeRef, dir: Dir) -> bool {
        let Some(path) = self.path(r) else { return false };
        if path.is_empty() {
            return false;
        }
        let fwd = dir.forward();
        let parent_depth = path.len() - 1;
        for depth in (0..path.len()).rev() {
            let Some(c) = self.container(&path[..depth]) else { return false };
            if c.kind.horizontal() != dir.horizontal() {
                continue;
            }
            let i = path[depth];
            let j = if fwd { i + 1 } else { i.wrapping_sub(1) };
            if j >= c.children.len() {
                continue;
            }
            let cid = c.id;
            if depth == parent_depth {
                // Where an entered container takes the node: its near edge when laid out
                // along `dir`, else after its focused child.
                let enter = match &c.children[j] {
                    Node::Window(_) => None,
                    Node::Container(t) => {
                        let along = t.kind.horizontal() == dir.horizontal() && !t.kind.single();
                        Some((t.id, if along { if fwd { 0 } else { usize::MAX } } else { t.focus + 1 }))
                    }
                };
                match enter {
                    None => {
                        let c = self.container_mut(&path[..depth]).unwrap();
                        c.children.swap(i, j);
                        c.sizes.swap(i, j);
                        c.focus = j;
                    }
                    Some((tid, at)) => {
                        let node = self.remove(r).unwrap();
                        self.insert(tid, at, node);
                    }
                }
                return true;
            }
            // `r` sits deeper: lift it out next to the subtree that contains it. That subtree
            // keeps its index even if it collapses to a single child.
            let node = self.remove(r).unwrap();
            if self.path(NodeRef::Container(cid)).is_none() {
                self.root.insert(if fwd { self.root.children.len() } else { 0 }, node);
                self.normalize();
                return true;
            }
            self.insert(cid, if fwd { i + 1 } else { i }, node);
            return true;
        }
        // At the workspace edge: lay the root out along `dir` with `r` at that edge.
        let kind = if dir.horizontal() { Kind::SplitH } else { Kind::SplitV };
        let only_child = self.root.children.len() == 1 && path.len() == 1;
        if only_child {
            return false;
        }
        let node = self.remove(r).unwrap();
        if self.root.kind != kind {
            let old = std::mem::replace(&mut self.root, Container::new(kind, Vec::new()));
            let rest = if old.children.len() == 1 { old.children.into_iter().next().unwrap() } else { Node::Container(old) };
            self.root.children.push(rest);
            self.root.sizes.push(1.0);
        }
        let at = if fwd { self.root.children.len() } else { 0 };
        self.root.insert(at, node);
        self.normalize();
        true
    }

    /// Grow (`delta` > 0) or shrink `r` along `horizontal`, taking space from a neighbor in
    /// the nearest container split that way.
    pub fn resize(&mut self, r: NodeRef, horizontal: bool, delta: f32) -> bool {
        let Some(path) = self.path(r) else { return false };
        for depth in (0..path.len()).rev() {
            let Some(c) = self.container_mut(&path[..depth]) else { return false };
            if c.kind.single() || c.kind.horizontal() != horizontal || c.children.len() < 2 {
                continue;
            }
            let i = path[depth];
            let j = if i + 1 < c.children.len() { i + 1 } else { i - 1 };
            let d = delta.clamp(-(c.sizes[i] - 0.05), c.sizes[j] - 0.05);
            c.sizes[i] += d;
            c.sizes[j] -= d;
            return true;
        }
        false
    }

    /// Set the boundary after child `i` of container `cid` to `frac` of the container.
    pub fn set_divider(&mut self, cid: u64, i: usize, frac: f32) {
        let Some(path) = self.path(NodeRef::Container(cid)) else { return };
        let Some(c) = self.container_mut(&path) else { return };
        if i + 1 >= c.sizes.len() {
            return;
        }
        let before: f32 = c.sizes[..i].iter().sum();
        let pair = c.sizes[i] + c.sizes[i + 1];
        let a = (frac - before).clamp(0.05, pair - 0.05);
        c.sizes[i] = a;
        c.sizes[i + 1] = pair - a;
    }

    pub fn balance(&mut self) {
        fn walk(c: &mut Container) {
            let n = c.children.len().max(1) as f32;
            c.sizes.iter_mut().for_each(|s| *s = 1.0 / n);
            for ch in &mut c.children {
                if let Node::Container(cc) = ch {
                    walk(cc);
                }
            }
        }
        walk(&mut self.root);
    }

    // ------------------------------------------------------------------ data

    /// The tree as plain data: windows map to `leaf(w)`.
    pub fn to_json(&self, leaf: &dyn Fn(WindowId) -> Value) -> Value {
        fn node(n: &Node, leaf: &dyn Fn(WindowId) -> Value) -> Value {
            match n {
                Node::Window(w) => leaf(*w),
                Node::Container(c) => container(c, leaf),
            }
        }
        fn container(c: &Container, leaf: &dyn Fn(WindowId) -> Value) -> Value {
            json!({
                "layout": c.kind.name(),
                "sizes": c.sizes,
                "focus": c.focus,
                "children": c.children.iter().map(|n| node(n, leaf)).collect::<Vec<_>>(),
            })
        }
        container(&self.root, leaf)
    }

    /// Build a tree from [`Self::to_json`] data (or the older binary `split`/`a`/`b` form);
    /// `leaf` turns a leaf object into a window.
    pub fn from_json(v: &Value, leaf: &mut dyn FnMut(&Value) -> WindowId) -> anyhow::Result<LayoutTree> {
        fn node(v: &Value, leaf: &mut dyn FnMut(&Value) -> WindowId) -> anyhow::Result<Node> {
            if v.get("buffer").is_some() {
                return Ok(Node::Window(leaf(v)));
            }
            if let Some(split) = v.get("split").and_then(Value::as_str) {
                let kind = if split == "below" { Kind::SplitV } else { Kind::SplitH };
                let ratio = v.get("ratio").and_then(Value::as_f64).unwrap_or(0.5) as f32;
                let a = node(v.get("a").ok_or_else(|| anyhow::anyhow!("split without a"))?, leaf)?;
                let b = node(v.get("b").ok_or_else(|| anyhow::anyhow!("split without b"))?, leaf)?;
                let mut c = Container::new(kind, vec![a, b]);
                c.sizes = vec![ratio, 1.0 - ratio];
                return Ok(Node::Container(c));
            }
            let kind = v.get("layout").and_then(Value::as_str).and_then(Kind::parse).unwrap_or(Kind::SplitH);
            let children = v
                .get("children")
                .and_then(Value::as_array)
                .ok_or_else(|| anyhow::anyhow!("container without children"))?
                .iter()
                .map(|c| node(c, leaf))
                .collect::<anyhow::Result<Vec<_>>>()?;
            let mut c = Container::new(kind, children);
            if let Some(s) = v.get("sizes").and_then(Value::as_array) {
                let s: Vec<f32> = s.iter().filter_map(Value::as_f64).map(|x| x as f32).collect();
                if s.len() == c.children.len() {
                    c.sizes = s;
                }
            }
            c.focus = v.get("focus").and_then(Value::as_u64).unwrap_or(0) as usize;
            Ok(Node::Container(c))
        }
        let root = match node(v, leaf)? {
            Node::Container(c) => c,
            w @ Node::Window(_) => Container::new(Kind::SplitH, vec![w]),
        };
        if root.children.is_empty() {
            anyhow::bail!("empty layout");
        }
        let mut t = LayoutTree { root, fullscreen: None };
        t.normalize();
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use slotmap::SlotMap;

    fn windows(n: usize) -> Vec<WindowId> {
        let mut m: SlotMap<WindowId, ()> = SlotMap::with_key();
        (0..n).map(|_| m.insert(())).collect()
    }

    /// Compact shape: `H[a V[b c]]`, windows lettered by creation order.
    fn shape(t: &LayoutTree, ws: &[WindowId]) -> String {
        fn node(n: &Node, ws: &[WindowId]) -> String {
            match n {
                Node::Window(w) => ((b'a' + ws.iter().position(|x| x == w).unwrap() as u8) as char).to_string(),
                Node::Container(c) => container(c, ws),
            }
        }
        fn container(c: &Container, ws: &[WindowId]) -> String {
            let k = match c.kind {
                Kind::SplitH => "H",
                Kind::SplitV => "V",
                Kind::Tabbed => "T",
                Kind::Stacked => "S",
            };
            format!("{k}[{}]", c.children.iter().map(|n| node(n, ws)).collect::<Vec<_>>().join(" "))
        }
        container(&t.root, ws)
    }

    #[test]
    fn split_appends_or_wraps() {
        let w = windows(4);
        let mut t = LayoutTree::new(w[0]);
        t.split(w[0], w[1], Kind::SplitH);
        t.split(w[1], w[2], Kind::SplitH);
        assert_eq!(shape(&t, &w), "H[a b c]");
        t.split(w[1], w[3], Kind::SplitV);
        assert_eq!(shape(&t, &w), "H[a V[b d] c]");
        let s: f32 = t.root.sizes.iter().sum();
        assert!((s - 1.0).abs() < 1e-4);
    }

    #[test]
    fn first_split_sets_orientation() {
        let w = windows(2);
        let mut t = LayoutTree::new(w[0]);
        t.split(w[0], w[1], Kind::SplitV);
        assert_eq!(shape(&t, &w), "V[a b]");
    }

    #[test]
    fn remove_flattens() {
        let w = windows(3);
        let mut t = LayoutTree::new(w[0]);
        t.split(w[0], w[1], Kind::SplitH);
        t.split(w[1], w[2], Kind::SplitV);
        t.remove(NodeRef::Window(w[2]));
        assert_eq!(shape(&t, &w), "H[a b]");
        t.remove(NodeRef::Window(w[0]));
        assert_eq!(shape(&t, &w), "H[b]");
    }

    #[test]
    fn move_swaps_enters_and_leaves() {
        let w = windows(4);
        let mut t = LayoutTree::new(w[0]);
        t.split(w[0], w[1], Kind::SplitH);
        t.split(w[1], w[2], Kind::SplitV);
        t.split(w[0], w[3], Kind::SplitH);
        assert_eq!(shape(&t, &w), "H[a d V[b c]]");
        // Swap with a window neighbor.
        assert!(t.move_node(NodeRef::Window(w[0]), Dir::Right));
        assert_eq!(shape(&t, &w), "H[d a V[b c]]");
        // Into a perpendicular container: after its focused child.
        assert!(t.move_node(NodeRef::Window(w[0]), Dir::Right));
        assert_eq!(shape(&t, &w), "H[d V[b c a]]");
        // Out of it again, to the left of the container.
        assert!(t.move_node(NodeRef::Window(w[0]), Dir::Left));
        assert_eq!(shape(&t, &w), "H[d a V[b c]]");
        // At the right edge going up: the root becomes vertical.
        assert!(t.move_node(NodeRef::Window(w[3]), Dir::Up));
        assert_eq!(shape(&t, &w), "V[d H[a V[b c]]]");
    }

    #[test]
    fn move_out_of_collapsing_container() {
        let w = windows(4);
        let mut t = LayoutTree::new(w[0]);
        t.split(w[0], w[1], Kind::SplitH);
        t.split(w[1], w[2], Kind::SplitV);
        t.split(w[0], w[3], Kind::SplitH);
        // H[a d V[b c]]: c moves right out of the column, which collapses to b.
        assert!(t.move_node(NodeRef::Window(w[2]), Dir::Right));
        assert_eq!(shape(&t, &w), "H[a d b c]");
    }

    #[test]
    fn place_edges_center_and_tabs() {
        let w = windows(4);
        let mut t = LayoutTree::new(w[0]);
        t.split(w[0], w[1], Kind::SplitH);
        t.split(w[1], w[2], Kind::SplitH);
        assert_eq!(shape(&t, &w), "H[a b c]");
        // Edge along the container: beside the target.
        assert!(t.place(NodeRef::Window(w[2]), NodeRef::Window(w[0]), Zone::Edge(Dir::Left)));
        assert_eq!(shape(&t, &w), "H[c a b]");
        // Edge across it: wrap the target.
        assert!(t.place(NodeRef::Window(w[2]), NodeRef::Window(w[1]), Zone::Edge(Dir::Down)));
        assert_eq!(shape(&t, &w), "H[a V[b c]]");
        // Center: a tab group, then joining it.
        assert!(t.place(NodeRef::Window(w[0]), NodeRef::Window(w[1]), Zone::Center));
        assert_eq!(shape(&t, &w), "V[T[b a] c]");
        // Never into itself.
        let tab = t.parent_of(NodeRef::Window(w[0])).unwrap();
        assert!(!t.place(NodeRef::Container(tab), NodeRef::Window(w[0]), Zone::Center));
    }

    #[test]
    fn place_in_reorders_two_tabs() {
        let w = windows(2);
        let mut t = LayoutTree::new(w[0]);
        t.split(w[0], w[1], Kind::SplitH);
        t.root.kind = Kind::Tabbed;
        let root = t.root.id;
        assert!(t.place_in(NodeRef::Window(w[1]), root, 0));
        assert_eq!(shape(&t, &w), "T[b a]");
        assert_eq!(t.root.focus, 0);
    }

    #[test]
    fn move_containers() {
        let w = windows(3);
        let mut t = LayoutTree::new(w[0]);
        t.split(w[0], w[1], Kind::SplitH);
        t.split(w[1], w[2], Kind::SplitV);
        let c = t.parent_of(NodeRef::Window(w[1])).unwrap();
        assert!(t.move_node(NodeRef::Container(c), Dir::Left));
        assert_eq!(shape(&t, &w), "H[V[b c] a]");
    }

    #[test]
    fn focus_follows_tree() {
        let w = windows(3);
        let mut t = LayoutTree::new(w[0]);
        t.split(w[0], w[1], Kind::SplitH);
        t.split(w[1], w[2], Kind::SplitV);
        t.note_focus(w[2]);
        assert_eq!(t.focus_dir(w[0], Dir::Right), Some(w[2]));
        assert_eq!(t.focus_dir(w[2], Dir::Up), Some(w[1]));
        assert_eq!(t.focus_dir(w[2], Dir::Left), Some(w[0]));
        assert_eq!(t.focus_dir(w[0], Dir::Left), None);
    }

    #[test]
    fn tabbed_shows_focused_child() {
        let w = windows(3);
        let mut t = LayoutTree::new(w[0]);
        t.split(w[0], w[1], Kind::SplitH);
        t.split(w[1], w[2], Kind::SplitH);
        t.root.kind = Kind::Tabbed;
        t.note_focus(w[1]);
        assert_eq!(t.visible_leaves(), vec![w[1]]);
        assert_eq!(t.focus_dir(w[1], Dir::Right), Some(w[2]));
    }

    #[test]
    fn json_roundtrip_and_legacy() {
        let w = windows(3);
        let mut t = LayoutTree::new(w[0]);
        t.split(w[0], w[1], Kind::SplitH);
        t.split(w[1], w[2], Kind::SplitV);
        let v = t.to_json(&|x| json!({"buffer": w.iter().position(|y| *y == x).unwrap()}));
        let mut next = w.iter().copied();
        let back = LayoutTree::from_json(&v, &mut |_| next.next().unwrap()).unwrap();
        assert_eq!(shape(&back, &w), "H[a V[b c]]");
        let legacy = json!({"split": "right", "ratio": 0.3, "a": {"buffer": 0}, "b": {"split": "below", "a": {"buffer": 1}, "b": {"buffer": 2}}});
        let mut next = w.iter().copied();
        let t = LayoutTree::from_json(&legacy, &mut |_| next.next().unwrap()).unwrap();
        assert_eq!(shape(&t, &w), "H[a V[b c]]");
        assert!((t.root.sizes[0] - 0.3).abs() < 1e-4);
    }
}
