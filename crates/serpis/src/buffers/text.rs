//! Plain text buffers: `*scratch*`, files, and read-only pages such as `*Messages*` and help.

use std::path::PathBuf;

use egui::Rect;

use crate::buffer::{Buffer, Modeline};
use crate::editor::Editor;
use crate::impl_any;
use crate::text::{CursorShape, PlainLines, TextArea, TextSurface, ViewOptions};
use crate::theme::Face;

pub struct TextBuffer {
    pub area: TextArea,
    pub path: Option<PathBuf>,
    pub major: String,
}

impl TextBuffer {
    pub fn new(text: &str) -> Self {
        let mut area = TextArea::new(text);
        area.view.wrap = false;
        TextBuffer {
            area,
            path: None,
            major: "text".into(),
        }
    }

    pub fn scratch() -> Self {
        Self::new(
            ";; *scratch* — a plain text buffer.\n;; M-x (or s-P / : in vim) runs any command. Try `new-chat`, `new-terminal`,\n;; `set-keybinding-scheme`, `describe-keymap`, `open-config`.\n",
        )
    }

    /// A read-only page (help, messages).
    pub fn page(text: &str, major: &str) -> Self {
        let mut b = Self::new(text);
        b.area.read_only = true;
        b.area.view.wrap = true;
        b.major = major.into();
        b
    }

    pub fn open(path: PathBuf) -> anyhow::Result<Self> {
        let text = match std::fs::read(&path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.into()),
        };
        let mut b = Self::new(&text.replace("\r\n", "\n"));
        b.path = Some(path);
        Ok(b)
    }

    pub fn save(&mut self) -> anyhow::Result<String> {
        let path = self.path.clone().ok_or_else(|| anyhow::anyhow!("Buffer is not visiting a file"))?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(&path, self.area.text())?;
        self.area.doc.modified = false;
        Ok(path.display().to_string())
    }
}

impl Buffer for TextBuffer {
    fn kind(&self) -> &str {
        &self.major
    }

    fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, focused: bool, ed: &mut Editor) {
        let id = ed.current_buffer();
        let shape = if focused { ed.cursor_shape(id) } else { None }.unwrap_or(CursorShape::Bar);
        let face = if self.area.read_only { Face::Default } else { Face::Code };
        let src = PlainLines { lines: &self.area.doc.lines, face };
        let selection = if focused { crate::packages::vim::selection_kind(ed, id) } else { Default::default() };
        self.area.view.show(ui, rect, &src, &ed.theme, &ViewOptions {
            focused,
            cursor: shape,
            show_cursor: true,
            selection,
        });
    }

    fn handle_text(&mut self, text: &str, _ed: &mut Editor) -> bool {
        if self.area.read_only {
            return false;
        }
        crate::commands::insert_replacing_selection(&mut self.area, text);
        true
    }

    fn surface(&mut self) -> Option<&mut dyn TextSurface> {
        Some(&mut self.area)
    }

    fn surface_ref(&self) -> Option<&dyn TextSurface> {
        Some(&self.area)
    }

    fn modeline(&self) -> Modeline {
        let c = self.area.view.cursor;
        let mut status = vec![format!("{}:{}", c.line + 1, c.col + 1)];
        if self.area.read_only {
            status.push("read-only".into());
        }
        Modeline {
            title: self.path.as_ref().map(|p| p.display().to_string()).unwrap_or_default(),
            status,
            busy: false,
            ..Default::default()
        }
    }

    fn directory(&self) -> Option<PathBuf> {
        self.path.as_ref().and_then(|p| p.parent()).map(|p| p.to_path_buf())
    }

    fn is_modified(&self) -> bool {
        self.path.is_some() && self.area.doc.modified
    }

    impl_any!();
}
