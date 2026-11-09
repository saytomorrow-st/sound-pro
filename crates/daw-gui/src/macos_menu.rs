//! Native macOS menu bar. egui's in-window menu is not shown on macOS; the
//! same actions live in the system menu at the top of the screen instead.

use std::sync::mpsc::{Receiver, Sender, channel};

use muda::accelerator::{Accelerator, Code, Modifiers};
use muda::{Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu};

use super::App;
use eframe::egui;

/// Menu actions, identified by the id of the item that raised them.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    About,
    Settings,
    Quit,
    New,
    Open,
    Save,
    SaveAs,
    Import,
    Export,
    NewTracks,
    NewMidi,
    Undo,
    Redo,
    Split,
    Copy,
    Paste,
    Duplicate,
    Library,
    Credits,
}

pub struct NativeMenu {
    _menu: Menu,
    items: Vec<(Action, MenuItem)>,
    events: Receiver<MenuId>,
    /// Last enabled flags pushed to the OS, to avoid redundant calls.
    shown: [bool; 5],
}

impl NativeMenu {
    pub fn install(ctx: &egui::Context) -> Self {
        let (tx, events): (Sender<MenuId>, _) = channel();
        let repaint = ctx.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let _ = tx.send(e.id);
            repaint.request_repaint();
        }));

        let menu = Menu::new();
        let mut items = Vec::new();
        let mut item = |label: &str, action: Action, accel: Option<Accelerator>| {
            let it = MenuItem::new(label, true, accel);
            items.push((action, it.clone()));
            it
        };
        let cmd = |code| Some(Accelerator::new(Some(Modifiers::META), code));
        let cmd_shift = |code| {
            Some(Accelerator::new(
                Some(Modifiers::META | Modifiers::SHIFT),
                code,
            ))
        };

        let app = Submenu::new("Sound Pro", true);
        let _ = app.append_items(&[
            &item("О программе", Action::About, None),
            &PredefinedMenuItem::separator(),
            &item("Настройки звука…", Action::Settings, cmd(Code::Comma)),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::services(None),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(None),
            &PredefinedMenuItem::hide_others(None),
            &PredefinedMenuItem::show_all(None),
            &PredefinedMenuItem::separator(),
            &item("Выйти из Sound Pro", Action::Quit, cmd(Code::KeyQ)),
        ]);

        let file = Submenu::new("Файл", true);
        let _ = file.append_items(&[
            &item("Новый", Action::New, cmd(Code::KeyN)),
            &item("Открыть…", Action::Open, cmd(Code::KeyO)),
            &item("Сохранить", Action::Save, cmd(Code::KeyS)),
            &item("Сохранить как…", Action::SaveAs, cmd_shift(Code::KeyS)),
            &PredefinedMenuItem::separator(),
            &item("Импорт WAV…", Action::Import, None),
            &item("Экспорт сведения…", Action::Export, None),
        ]);

        let edit = Submenu::new("Правка", true);
        let _ = edit.append_items(&[
            &item("Отменить", Action::Undo, None),
            &item("Повторить", Action::Redo, None),
            &PredefinedMenuItem::separator(),
            &item("Разрезать по курсору", Action::Split, None),
            &item("Копировать клип", Action::Copy, None),
            &item("Вставить клип", Action::Paste, None),
            &item("Дублировать клип", Action::Duplicate, None),
        ]);

        let track = Submenu::new("Дорожка", true);
        let _ = track.append_items(&[
            &item("Новые дорожки…", Action::NewTracks, None),
            &item("Новая MIDI-дорожка", Action::NewMidi, None),
        ]);

        let window = Submenu::new("Окно", true);
        let _ = window.append_items(&[
            &PredefinedMenuItem::minimize(None),
            &PredefinedMenuItem::maximize(None),
        ]);

        let help = Submenu::new("Справка", true);
        let _ = help.append_items(&[
            &item("Библиотека звуков…", Action::Library, None),
            &item("Авторы…", Action::Credits, None),
        ]);

        let _ = menu.append_items(&[&app, &file, &edit, &track, &window, &help]);
        menu.init_for_nsapp();
        window.set_as_windows_menu_for_nsapp();
        help.set_as_help_menu_for_nsapp();

        Self {
            _menu: menu,
            items,
            events,
            shown: [true; 5],
        }
    }

    fn action(&self, id: &MenuId) -> Option<Action> {
        self.items
            .iter()
            .find(|(_, it)| it.id() == id)
            .map(|(a, _)| *a)
    }

    fn set_enabled(&mut self, flags: [bool; 5]) {
        if flags == self.shown {
            return;
        }
        self.shown = flags;
        let targets = [
            Action::Undo,
            Action::Redo,
            Action::Split,
            Action::Copy,
            Action::Paste,
        ];
        for (action, on) in targets.into_iter().zip(flags) {
            if let Some((_, it)) = self.items.iter().find(|(a, _)| *a == action) {
                it.set_enabled(on);
            }
        }
    }
}

impl App {
    /// Applies pending native-menu clicks and refreshes enabled states.
    pub(super) fn poll_native_menu(&mut self, ctx: &egui::Context) {
        let Some(mut menu) = self.native_menu.take() else {
            return;
        };
        let has_clip = self.editor.selected_clip.is_some();
        menu.set_enabled([
            self.editor.can_undo(),
            self.editor.can_redo(),
            has_clip,
            has_clip,
            self.editor.has_clipboard(),
        ]);
        let mut actions = Vec::new();
        while let Ok(id) = menu.events.try_recv() {
            actions.extend(menu.action(&id));
        }
        self.native_menu = Some(menu);
        for action in actions {
            self.run_menu_action(action, ctx);
        }
    }

    fn run_menu_action(&mut self, action: Action, ctx: &egui::Context) {
        match action {
            Action::About => self.about_open = true,
            Action::Settings => self.open_audio_settings(),
            Action::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::New => {
                self.reset_playback();
                self.editor = super::Editor::new();
                self.status = "Новый проект".into();
            }
            Action::Open => self.open_dialog(),
            Action::Save => self.save(),
            Action::SaveAs => self.save_as_dialog(),
            Action::Import => self.import_dialog(),
            Action::Export => self.export_dialog(),
            Action::NewTracks => self.open_new_track(super::TrackKind::Audio),
            Action::NewMidi => self.add_midi_track(),
            Action::Undo => self.undo(),
            Action::Redo => self.redo(),
            Action::Split => self.split_clip(),
            Action::Copy => self.copy_clip(ctx),
            Action::Paste => self.paste_clip(),
            Action::Duplicate => self.duplicate_clip(),
            Action::Library => self.open_library(),
            Action::Credits => self.credits_open = true,
        }
    }
}
