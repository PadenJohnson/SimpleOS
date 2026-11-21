//! The document apps built on the shared shell: Writer, PDF and the Text Editor.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::desktop::App;
use crate::docshell::{has_ext, DocApp, DocShell};

pub fn all() -> Vec<(App, Box<dyn DocApp>)> {
    Vec::from([
        (App::Writer, DocShell::new(&crate::writer::KIND, crate::writer::new_doc, crate::writer::load) as Box<dyn DocApp>),
        (App::TextEdit, DocShell::new(&crate::textedit::KIND, crate::textedit::new_doc, crate::textedit::load) as Box<dyn DocApp>),
    ])
}

/// Does one of them open this file?
pub fn opens_any(name: &str) -> bool {
    has_ext(name, crate::writer::KIND.exts) || has_ext(name, crate::textedit::KIND.exts)
}
