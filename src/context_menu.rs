//! right-click menus. the app builds the rows for where the click was; the
//! menu shows them with their shortcuts and hands back the picked action.
//! same shape as lunia's.

use aurea::render::Point;
use aurea::ui::{MenuItem, Ui};
use aurea::{Chord, Keymap};

use crate::app::{Command, Id};
use crate::lang::Texts;

/// what a row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Act {
    Run(Command),
    NewFile,
    NewFolder,
    Rename,
    Delete,
    Reveal,
    /// opens the file and shows its preview.
    Preview,
    CopyPath,
    CloseOthers,
    CloseAll,
}

/// one row, or a line between groups.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    Item(&'static str, Act),
    Line,
}

/// a menu asked for, then the one up.
#[derive(Debug, Default)]
pub struct ContextMenu {
    asked: Option<(Point, Vec<Entry>)>,
    /// the act behind each row of the open menu, in row order.
    shown: Vec<Option<Act>>,
    pub open: bool,
}

impl ContextMenu {
    /// opens `entries` at `at` on the next frame.
    pub fn ask(&mut self, at: Point, entries: Vec<Entry>) {
        self.asked = Some((at, entries));
    }

    /// opens an asked menu and takes a pick. the act picked, once.
    pub fn show(
        &mut self,
        ui: &mut Ui<'_, Id>,
        texts: &Texts,
        keymap: &Keymap<Command>,
    ) -> Option<Act> {
        if let Some((at, entries)) = self.asked.take() {
            let labels: Vec<(String, String)> = entries
                .iter()
                .map(|e| match e {
                    Entry::Item(label, act) => {
                        let chord = match act {
                            Act::Run(command) => {
                                keymap.chord_for(command).map(|c: Chord| c.label())
                            }
                            _ => None,
                        };
                        (texts.tr(label).to_owned(), chord.unwrap_or_default())
                    }
                    Entry::Line => (String::new(), String::new()),
                })
                .collect();
            let items: Vec<MenuItem<'_>> = entries
                .iter()
                .zip(&labels)
                .map(|(e, (label, chord))| match e {
                    Entry::Item(..) => MenuItem::new(label).shortcut(chord),
                    Entry::Line => MenuItem::separator(),
                })
                .collect();
            self.shown = entries
                .iter()
                .map(|e| match e {
                    Entry::Item(_, act) => Some(*act),
                    Entry::Line => None,
                })
                .collect();
            ui.open_menu(Id::Menu, at, items);
            self.open = true;
        }
        if !ui.menu_open(Id::Menu) {
            self.open = false;
        }
        let picked = ui.menu_picked(Id::Menu)?;
        self.open = false;
        self.shown.get(picked).copied().flatten()
    }
}

/// rows for a right click on an explorer entry, or on its empty space.
pub fn explorer(on_entry: bool, previews: bool) -> Vec<Entry> {
    let mut rows = vec![
        Entry::Item("New File", Act::NewFile),
        Entry::Item("New Folder", Act::NewFolder),
    ];
    if previews {
        rows.insert(0, Entry::Line);
        rows.insert(0, Entry::Item("Open Preview", Act::Preview));
    }
    if on_entry {
        rows.extend([
            Entry::Line,
            Entry::Item("Rename", Act::Rename),
            Entry::Item("Delete", Act::Delete),
            Entry::Line,
            Entry::Item("Copy Path", Act::CopyPath),
            Entry::Item("Reveal in File Manager", Act::Reveal),
        ]);
    }
    rows
}

/// rows for a right click in an editor.
pub fn editor() -> Vec<Entry> {
    use crate::editor::LineOp;
    vec![
        Entry::Item("Quick Fix", Act::Run(Command::CodeActions)),
        Entry::Item("Preview", Act::Run(Command::TogglePreview)),
        Entry::Item("Ask Agent About This", Act::Run(Command::AskAgent)),
        Entry::Item("Go to Definition", Act::Run(Command::GotoDefinition)),
        Entry::Item("Find References", Act::Run(Command::References)),
        Entry::Item("Rename Symbol", Act::Run(Command::Rename)),
        Entry::Item("Format Document", Act::Run(Command::Format)),
        Entry::Line,
        Entry::Item("Cut", Act::Run(Command::Cut)),
        Entry::Item("Copy", Act::Run(Command::Copy)),
        Entry::Item("Paste", Act::Run(Command::Paste)),
        Entry::Line,
        Entry::Item("Toggle Comment", Act::Run(Command::Line(LineOp::Comment))),
        Entry::Item("Command Palette", Act::Run(Command::Commands)),
    ]
}

/// rows for a right click on a tab.
pub fn tab(editor: bool) -> Vec<Entry> {
    let mut rows = vec![
        Entry::Item("Close Tab", Act::Run(Command::CloseTab)),
        Entry::Item("Close Others", Act::CloseOthers),
        Entry::Item("Close All", Act::CloseAll),
    ];
    if editor {
        rows.extend([
            Entry::Line,
            Entry::Item("Split Editor", Act::Run(Command::SplitEditor)),
            Entry::Item("Copy Path", Act::CopyPath),
            Entry::Item("Reveal in File Manager", Act::Reveal),
        ]);
    }
    rows
}
