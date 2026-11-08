//! A plugin's settings page: every setting its manifest declares, edited in place and saved
//! together (`Engine::save_settings`). The page replaces the list; the search text stays
//! underneath and comes back on Esc.
//!
//! The values live here, in [`SettingsView`]; the Slint rows only report edits. A row's text
//! field keeps what was typed, so the model is rebuilt as a whole (fresh rows) only when Rust
//! changes a value itself: reset, save.
//!
//! A table with `fields` lists its entries; Add or Edit swaps the page for a sub-form with one
//! row per field (the same editors), which writes the entry back into the table.

use std::rc::Rc;

use slint::{Model, ModelRc, SharedString, VecModel};

use super::{App, MainWindow, SettingField, with_state};
use crate::engine::{PluginInfo, SettingChange};
use crate::plugin::manifest::{Setting, SettingKind};

/// `SettingField::kind` values, as in `ui/settings.slint`.
mod kind {
    pub const TEXT: i32 = 0;
    pub const NUMBER: i32 = 1;
    pub const BOOL: i32 = 2;
    pub const CHOICE: i32 = 3;
    pub const SECRET: i32 = 4;
    pub const SECRETS: i32 = 5;
    pub const LINES: i32 = 6;
    pub const TOML: i32 = 7;
    pub const READ_ONLY: i32 = 8;
    pub const ENTRIES: i32 = 9;
    pub const PAIRS: i32 = 10;
}

/// The open settings page.
pub(super) struct SettingsView {
    plugin: String,
    title: String,
    fields: Vec<FieldState>,
    /// The entry being added or edited, shown instead of `fields`.
    editing: Option<EntryEdit>,
    /// Esc was pressed once with unsaved changes; the next one discards them.
    warned: bool,
    saving: bool,
}

impl SettingsView {
    /// The rows on screen: the entry's while one is open, else the page's.
    fn active(&self) -> &[FieldState] {
        self.editing.as_ref().map_or(&self.fields, |e| &e.fields)
    }

    fn active_mut(&mut self) -> &mut Vec<FieldState> {
        match self.editing.as_mut() {
            Some(e) => &mut e.fields,
            None => &mut self.fields,
        }
    }

    fn page_title(&self) -> String {
        match &self.editing {
            Some(e) => format!("{} › {}", self.title, e.original.as_deref().unwrap_or(&e.fields[0].original)),
            None => self.title.clone(),
        }
    }

    /// Write the open entry into its table and go back to the page; on a problem the entry
    /// stays open with the message on its field.
    fn apply_entry(&mut self) -> Result<(), ()> {
        let Some(edit) = self.editing.as_mut() else { return Ok(()) };
        let parent = &self.fields[edit.field];
        let mut table = parent.table();
        match edit.entry(&table) {
            Ok((name, entry)) => {
                if let Some(old) = &edit.original {
                    table.remove(old);
                }
                table.insert(name, toml::Value::Table(entry));
                let field = edit.field;
                self.editing = None;
                let parent = &mut self.fields[field];
                parent.set_table(&table);
                parent.reset = false;
                parent.error.clear();
                Ok(())
            }
            Err((i, msg)) => {
                edit.fields[i].error = msg;
                Err(())
            }
        }
    }
}

/// An entry of a table with `fields`, open in the sub-form.
struct EntryEdit {
    /// The table's index in `SettingsView::fields`.
    field: usize,
    /// The entry's name in the table; `None` for a new one.
    original: Option<String>,
    /// The name, then one row per field.
    fields: Vec<FieldState>,
    /// The entry as stored (keys the manifest does not declare are kept).
    stored: toml::Table,
}

impl EntryEdit {
    fn new(plugin: &str, index: usize, parent: &FieldState, name: &str) -> Self {
        let table = parent.table();
        let stored = table.get(name).and_then(toml::Value::as_table).cloned();
        let name_setting = Setting {
            key: String::new(),
            kind: SettingKind::String,
            label: "Name".into(),
            description: String::new(),
            default: None,
            options: vec![],
            example: String::new(),
            placeholder: String::new(),
            required: true,
            fields: vec![],
            import: None,
        };
        let mut fields = vec![FieldState::new(plugin, &name_setting, Some(&serde_json::json!(name)))];
        for sub in &parent.setting.fields {
            let value = stored.as_ref().and_then(|e| e.get(&sub.key)).and_then(|v| serde_json::to_value(v).ok());
            fields.push(FieldState::new(plugin, sub, value.as_ref()));
        }
        // "Reset to default" means nothing for one entry: clearing a field leaves it out.
        for f in &mut fields {
            f.user_set = false;
        }
        Self {
            field: index,
            original: stored.is_some().then(|| name.to_string()),
            fields,
            stored: stored.unwrap_or_default(),
        }
    }

    /// The entry's name and table, or the row and message of what is wrong.
    fn entry(&self, table: &toml::Table) -> Result<(String, toml::Table), (usize, String)> {
        let name = self.fields[0].text.trim().to_string();
        if name.is_empty() {
            return Err((0, "a name is needed".into()));
        }
        if self.original.as_deref() != Some(name.as_str()) && table.contains_key(&name) {
            return Err((0, format!("{name} exists already")));
        }
        let mut entry = self.stored.clone();
        for (i, f) in self.fields.iter().enumerate().skip(1) {
            let key = &f.setting.key;
            entry.remove(key);
            let default_bool = || f.setting.default.as_ref().and_then(toml::Value::as_bool).unwrap_or(false);
            if f.text.trim().is_empty() || (f.kind == kind::BOOL && (f.text == "true") == default_bool()) {
                if f.setting.required {
                    return Err((i, "required".into()));
                }
                continue;
            }
            match f.value() {
                Ok(Some(v)) => {
                    entry.insert(key.clone(), v);
                }
                Ok(None) => {}
                Err(e) => return Err((i, e)),
            }
        }
        Ok((name, entry))
    }
}

/// One setting on the page.
#[derive(Debug, Clone)]
struct FieldState {
    setting: Setting,
    kind: i32,
    /// The text as loaded, and as edited.
    original: String,
    text: String,
    /// Back to the default on save (for a secret: removed).
    reset: bool,
    /// Differs from the default (or, for a secret, is stored).
    user_set: bool,
    /// Named secrets: stored names, and the edits to them.
    names: Vec<String>,
    added: Vec<(String, String)>,
    removed: Vec<String>,
    error: String,
    /// What an empty field shows.
    placeholder: String,
}

impl FieldState {
    fn new(plugin: &str, setting: &Setting, current: Option<&serde_json::Value>) -> Self {
        let value = current.and_then(|v| toml::Value::try_from(v).ok());
        let default_text = setting.input_text(setting.default.as_ref());
        let kind = match setting.kind {
            SettingKind::String | SettingKind::Path => kind::TEXT,
            SettingKind::Choice if setting.options.is_empty() => kind::TEXT,
            SettingKind::Choice => kind::CHOICE,
            SettingKind::Number => kind::NUMBER,
            SettingKind::Bool => kind::BOOL,
            SettingKind::Secret => kind::SECRET,
            SettingKind::Secrets => kind::SECRETS,
            SettingKind::List if setting.is_line_list(value.as_ref()) => kind::LINES,
            SettingKind::Table if !setting.fields.is_empty() => kind::ENTRIES,
            SettingKind::Table if setting.is_pair_table(value.as_ref()) => kind::PAIRS,
            SettingKind::List | SettingKind::Table => kind::TOML,
            SettingKind::Other(_) => kind::READ_ONLY,
        };
        let (text, user_set, names) = match kind {
            kind::SECRET => (String::new(), current.is_some(), vec![]),
            kind::SECRETS => {
                let names: Vec<String> = current
                    .and_then(serde_json::Value::as_object)
                    .map(|o| o.keys().cloned().collect())
                    .unwrap_or_default();
                (String::new(), !names.is_empty(), names)
            }
            kind::BOOL => {
                let on = current.and_then(serde_json::Value::as_bool).unwrap_or(false);
                (on.to_string(), setting.default.as_ref().and_then(toml::Value::as_bool) != Some(on), vec![])
            }
            _ => {
                let text = setting.input_text(value.as_ref());
                let user_set = value.is_some() && text != default_text;
                (text, user_set, vec![])
            }
        };
        let placeholder = match kind {
            _ if !setting.placeholder.is_empty() => setting.placeholder.clone(),
            kind::TOML => setting.input_example(plugin),
            kind::ENTRIES if setting.import.is_some() => "name of a new entry, or paste JSON".into(),
            kind::NUMBER => default_text,
            _ => String::new(),
        };
        Self {
            setting: setting.clone(),
            kind,
            original: text.clone(),
            text,
            reset: false,
            user_set,
            names,
            added: vec![],
            removed: vec![],
            error: String::new(),
            placeholder,
        }
    }

    fn dirty(&self) -> bool {
        self.reset
            || !self.added.is_empty()
            || !self.removed.is_empty()
            || if self.kind == kind::SECRET { !self.text.is_empty() } else { self.text != self.original }
    }

    /// What saving this field asks for.
    fn changes(&self) -> Vec<SettingChange> {
        let key = self.setting.key.clone();
        let mut out = vec![];
        if self.kind == kind::SECRET {
            if !self.text.is_empty() {
                out.push(SettingChange::Set { key: key.clone(), text: self.text.clone() });
            } else if self.reset {
                out.push(SettingChange::Reset { key: key.clone() });
            }
        } else if self.reset {
            out.push(SettingChange::Reset { key: key.clone() });
        } else if self.text != self.original {
            out.push(SettingChange::Set { key: key.clone(), text: self.text.clone() });
        }
        for name in &self.removed {
            out.push(SettingChange::SecretEntry { key: key.clone(), name: name.clone(), value: String::new() });
        }
        for (name, value) in &self.added {
            out.push(SettingChange::SecretEntry { key: key.clone(), name: name.clone(), value: value.clone() });
        }
        out
    }

    /// The table an ENTRIES or PAIRS field holds (its text is that table's TOML).
    fn table(&self) -> toml::Table {
        toml::from_str(&self.text).unwrap_or_default()
    }

    fn set_table(&mut self, t: &toml::Table) {
        self.text = self.setting.input_text(Some(&toml::Value::Table(t.clone())));
    }

    /// Merge pasted text into this table (`Setting::import_entries`); the imported names.
    fn import(&mut self, raw: &str) -> Result<Vec<String>, String> {
        let (table, names) = self.setting.import_entries(Some(&toml::Value::Table(self.table())), raw)?;
        self.set_table(&table);
        self.reset = false;
        self.error.clear();
        Ok(names)
    }

    /// The typed value as TOML, checked like a save would (`Setting::parse_input`).
    fn value(&self) -> Result<Option<toml::Value>, String> {
        let Some(item) = self.setting.parse_input(&self.text)? else { return Ok(None) };
        let mut doc = toml_edit::DocumentMut::new();
        doc.insert("v", item);
        let mut t: toml::Table = toml::from_str(&doc.to_string()).map_err(|e| e.to_string())?;
        Ok(t.remove("v"))
    }

    /// One line about an entry of an ENTRIES field: its first field with a value.
    fn summary(&self, entry: &toml::Value) -> String {
        let show = |v: &toml::Value| match v {
            toml::Value::String(s) => s.clone(),
            toml::Value::Array(a) => {
                a.iter().map(|x| x.as_str().map_or_else(|| x.to_string(), str::to_string)).collect::<Vec<_>>().join(" ")
            }
            toml::Value::Table(t) => t.keys().cloned().collect::<Vec<_>>().join(", "),
            v => v.to_string(),
        };
        self.setting
            .fields
            .iter()
            .filter_map(|f| entry.get(&f.key).map(show))
            .find(|s| !s.trim().is_empty())
            .unwrap_or_default()
    }

    fn row(&self) -> SettingField {
        let s = &self.setting;
        let strings =
            |v: Vec<String>| ModelRc::new(VecModel::from(v.into_iter().map(SharedString::from).collect::<Vec<_>>()));
        let labels: Vec<String> =
            s.options.iter().map(|o| if o.is_empty() { "none".to_string() } else { o.clone() }).collect();
        let segmented = labels.len() <= 4 && labels.iter().map(|l| l.chars().count() + 2).sum::<usize>() <= 36;
        let (names, values): (Vec<String>, Vec<String>) = match self.kind {
            kind::ENTRIES => self.table().iter().map(|(k, v)| (k.clone(), self.summary(v))).unzip(),
            kind::PAIRS => self
                .table()
                .iter()
                .map(|(k, v)| (k.clone(), v.as_str().map_or_else(|| v.to_string(), str::to_string)))
                .unzip(),
            _ => {
                let mut names: Vec<String> = self.names.iter().filter(|n| !self.removed.contains(n)).cloned().collect();
                for (name, _) in &self.added {
                    if !names.contains(name) {
                        names.push(name.clone());
                    }
                }
                (names, vec![])
            }
        };
        let lines = self.text.lines().count().max(self.placeholder.lines().count());
        SettingField {
            key: s.key.as_str().into(),
            label: s.title().into(),
            description: s.description.as_str().into(),
            placeholder: self.placeholder.as_str().into(),
            kind: self.kind,
            text: self.text.as_str().into(),
            options: strings(s.options.clone()),
            option_labels: strings(labels),
            choice_index: s.options.iter().position(|o| *o == self.text).unwrap_or(0).try_into().unwrap_or(0),
            segmented,
            is_set: self.kind == kind::SECRET && self.user_set && !self.reset,
            names: strings(names),
            values: strings(values),
            dirty: self.dirty(),
            user_set: self.user_set && !self.reset,
            required: s.required,
            error: self.error.as_str().into(),
            rows: i32::try_from(lines.clamp(3, 10)).unwrap_or(3),
        }
    }
}

/// The page's fields for plugin `p`, in manifest order.
fn fields_of(p: &PluginInfo) -> Vec<FieldState> {
    p.manifest.settings.iter().map(|s| FieldState::new(&p.manifest.id, s, p.settings.get(&s.key))).collect()
}

impl App {
    /// Show the settings page of plugin `id` in place of the list.
    pub(super) fn open_settings(&self, ui: &MainWindow, id: &str) {
        let reg = self.engine.plugins();
        let Some(p) = reg.plugins.iter().find(|p| p.manifest.id == id) else {
            log::warn!("settings: no plugin {id:?}");
            return;
        };
        let fields = fields_of(p);
        let title = format!("{} settings", p.manifest.name);
        with_state(|s| {
            s.settings = Some(SettingsView {
                plugin: id.to_string(),
                title: title.clone(),
                fields,
                editing: None,
                warned: false,
                saving: false,
            });
        });
        ui.set_menu_open(false);
        ui.set_settings_title(title.into());
        ui.set_settings_status(SharedString::new());
        ui.set_settings_status_error(false);
        Self::show_settings_rows(ui);
        // The first field takes the keyboard when its row is created (Slint creates rows
        // lazily, at the next frame); rebuilds turn this off again.
        ui.set_settings_autofocus(true);
        ui.set_settings_open(true);
    }

    /// Rebuild every row (new rows, so text fields show the stored text again).
    fn show_settings_rows(ui: &MainWindow) {
        ui.set_settings_autofocus(false);
        let (rows, dirty, title) = with_state(|s| {
            let v = s.settings.as_ref()?;
            Some((
                v.active().iter().map(FieldState::row).collect::<Vec<_>>(),
                v.active().iter().filter(|f| f.dirty()).count(),
                v.page_title(),
            ))
        })
        .unwrap_or_default();
        ui.set_settings_title(title.into());
        ui.set_settings_fields(ModelRc::from(Rc::new(VecModel::from(rows))));
        ui.set_settings_dirty(i32::try_from(dirty).unwrap_or(i32::MAX));
        ui.invoke_focus_page();
    }

    /// Apply `edit` to field `index` and refresh that row only (a rebuild would drop the caret).
    fn edit_setting(&self, index: i32, rebuild: bool, edit: impl FnOnce(&mut FieldState)) {
        let Some(ui) = self.weak.upgrade() else { return };
        let Ok(index) = usize::try_from(index) else { return };
        let Some((row, dirty)) = with_state(|s| {
            let v = s.settings.as_mut()?;
            let f = v.active_mut().get_mut(index)?;
            edit(f);
            f.error.clear();
            let row = f.row();
            v.warned = false;
            Some((row, v.active().iter().filter(|f| f.dirty()).count()))
        }) else {
            return;
        };
        ui.set_settings_status(SharedString::new());
        ui.set_settings_status_error(false);
        if rebuild {
            Self::show_settings_rows(&ui);
        } else {
            ui.get_settings_fields().set_row_data(index, row);
            ui.set_settings_dirty(i32::try_from(dirty).unwrap_or(i32::MAX));
        }
    }

    pub(super) fn settings_edited(&self, index: i32, text: &str) {
        self.edit_setting(index, false, |f| {
            f.text = text.to_string();
            f.reset = false;
        });
    }

    /// Reset to the default (shown at once), or for a secret: remove it on save.
    pub(super) fn settings_reset(&self, index: i32) {
        self.edit_setting(index, true, |f| {
            f.reset = true;
            f.text =
                if f.kind == kind::SECRET { String::new() } else { f.setting.input_text(f.setting.default.as_ref()) };
        });
    }

    /// Add a named secret or a key/value pair (rebuilt: the row's item list changes).
    pub(super) fn settings_item_add(&self, index: i32, name: &str, value: &str) {
        let name = name.trim().to_string();
        self.edit_setting(index, true, |f| {
            if f.kind == kind::PAIRS {
                let mut t = f.table();
                t.insert(name, toml::Value::String(value.to_string()));
                f.set_table(&t);
                f.reset = false;
                return;
            }
            f.removed.retain(|n| *n != name);
            f.added.retain(|(n, _)| *n != name);
            f.added.push((name, value.to_string()));
        });
    }

    /// Remove a named secret (on save), a key/value pair or an entry.
    pub(super) fn settings_item_remove(&self, index: i32, name: &str) {
        self.edit_setting(index, true, |f| {
            if matches!(f.kind, kind::PAIRS | kind::ENTRIES) {
                let mut t = f.table();
                t.remove(name);
                f.set_table(&t);
                f.reset = false;
                return;
            }
            let before = f.added.len();
            f.added.retain(|(n, _)| n != name);
            if f.added.len() == before && !f.removed.iter().any(|n| n == name) {
                f.removed.push(name.to_string());
            }
        });
    }

    /// Open entry `name` of table field `index` in the sub-form (a new one if there is none).
    pub(super) fn settings_entry_open(&self, index: i32, name: &str) {
        let Some(ui) = self.weak.upgrade() else { return };
        let Ok(index) = usize::try_from(index) else { return };
        let imports = with_state(|s| {
            s.settings.as_ref().and_then(|v| v.fields.get(index)).is_some_and(|f| f.setting.imports(name))
        });
        if imports {
            return self.settings_import(index, name);
        }
        let opened = with_state(|s| {
            let v = s.settings.as_mut().filter(|v| v.editing.is_none())?;
            let parent = v.fields.get(index).filter(|f| f.kind == kind::ENTRIES)?;
            v.editing = Some(EntryEdit::new(&v.plugin, index, parent, name.trim()));
            v.warned = false;
            Some(())
        });
        if opened.is_some() {
            ui.set_settings_status(SharedString::new());
            ui.set_settings_status_error(false);
            Self::show_settings_rows(&ui);
            ui.set_settings_autofocus(true);
        }
    }

    /// Pasted text (see `Setting::import`) added to table field `index` as entries; saved with
    /// the page.
    fn settings_import(&self, index: usize, raw: &str) {
        let Some(ui) = self.weak.upgrade() else { return };
        let result = with_state(|s| {
            let f = s.settings.as_mut()?.fields.get_mut(index)?;
            Some(f.import(raw))
        });
        let Some(result) = result else { return };
        let (status, error) = match result {
            Ok(names) => (format!("Added {} · Save to keep them", names.join(", ")), false),
            Err(e) => (format!("Not added: {e}"), true),
        };
        Self::show_settings_rows(&ui);
        ui.set_settings_status(status.into());
        ui.set_settings_status_error(error);
    }

    /// Leave the sub-form without keeping its edits.
    fn close_entry(ui: &MainWindow) {
        with_state(|s| {
            s.settings.as_mut().map(|v| {
                v.editing = None;
                v.warned = false;
            })
        });
        ui.set_settings_status(SharedString::new());
        ui.set_settings_status_error(false);
        Self::show_settings_rows(ui);
    }

    /// Save every changed field at once; on success the page shows the stored values again.
    pub(super) fn save_settings(&self) {
        let Some(ui) = self.weak.upgrade() else { return };
        // An open entry goes into its table first, then everything is saved.
        let applied = with_state(|s| {
            let v = s.settings.as_mut().filter(|v| !v.saving)?;
            let was_open = v.editing.is_some();
            Some(v.apply_entry().map(|()| was_open))
        });
        match applied {
            Some(Ok(true)) => Self::show_settings_rows(&ui),
            Some(Err(())) => {
                Self::refresh_rows(&ui);
                ui.set_settings_status("Not saved: see the marked fields".into());
                ui.set_settings_status_error(true);
                return;
            }
            _ => {}
        }
        let Some((plugin, changes)) = with_state(|s| {
            let v = s.settings.as_mut().filter(|v| !v.saving)?;
            let changes: Vec<SettingChange> = v.fields.iter().flat_map(FieldState::changes).collect();
            v.saving = !changes.is_empty();
            Some((v.plugin.clone(), changes))
        }) else {
            return;
        };
        if changes.is_empty() {
            ui.set_settings_status("Nothing to save".into());
            return;
        }
        ui.set_settings_status("Saving…".into());
        ui.set_settings_status_error(false);
        let app = self.clone();
        std::thread::spawn(move || {
            // Writes config.toml and the secrets, then reloads (restarting the plugin).
            let result = app.engine.save_settings(&plugin, changes);
            let _ = slint::invoke_from_event_loop(move || app.settings_saved(&plugin, result));
        });
    }

    fn settings_saved(&self, plugin: &str, result: Result<(), Vec<(String, String)>>) {
        let Some(ui) = self.weak.upgrade() else { return };
        let still_open = with_state(|s| match s.settings.as_mut() {
            Some(v) if v.plugin == plugin => {
                v.saving = false;
                true
            }
            _ => false,
        });
        if !still_open {
            return;
        }
        match result {
            Ok(()) => {
                let reg = self.engine.plugins();
                let fields = reg.plugins.iter().find(|p| p.manifest.id == plugin).map(fields_of).unwrap_or_default();
                with_state(|s| s.settings.as_mut().map(|v| v.fields = fields));
                Self::show_settings_rows(&ui);
                ui.set_settings_status("Saved".into());
                ui.set_settings_status_error(false);
            }
            Err(errors) => {
                let general = with_state(|s| {
                    let v = s.settings.as_mut()?;
                    let mut general = None;
                    for (key, msg) in errors {
                        match v.fields.iter_mut().find(|f| f.setting.key == key) {
                            Some(f) => f.error = msg,
                            None => general = Some(msg),
                        }
                    }
                    general
                });
                Self::refresh_rows(&ui);
                ui.set_settings_status(general.unwrap_or_else(|| "Not saved: see the marked fields".into()).into());
                ui.set_settings_status_error(true);
            }
        }
    }

    /// Show the fields' messages, refreshing rows one by one: a rebuild would throw away what
    /// was typed.
    fn refresh_rows(ui: &MainWindow) {
        let rows = with_state(|s| {
            s.settings.as_ref().map(|v| v.active().iter().map(FieldState::row).collect::<Vec<_>>()).unwrap_or_default()
        });
        let model = ui.get_settings_fields();
        for (i, row) in rows.into_iter().enumerate() {
            model.set_row_data(i, row);
        }
    }

    /// Esc or ←: leave the entry or the page, after one warning if there are unsaved changes.
    pub(super) fn settings_back(&self) {
        let Some(ui) = self.weak.upgrade() else { return };
        let warn = with_state(|s| {
            let v = s.settings.as_mut()?;
            let warn = !v.warned && v.active().iter().any(FieldState::dirty);
            v.warned = warn;
            Some((warn, v.editing.is_some()))
        });
        match warn {
            Some((true, entry)) => {
                let what = if entry { "this entry's changes" } else { "them" };
                ui.set_settings_status(format!("Unsaved changes: Esc again to discard {what}").into());
                ui.set_settings_status_error(true);
            }
            Some((false, true)) => Self::close_entry(&ui),
            Some((false, false)) => {
                self.close_settings();
                // Back to the search that opened the page, with fresh rows (values changed).
                ui.invoke_focus_input();
                self.search(ui.get_query().to_string());
            }
            None => {}
        }
    }

    /// Drop the page (also when the window hides for good).
    pub(super) fn close_settings(&self) {
        let Some(ui) = self.weak.upgrade() else { return };
        if with_state(|s| s.settings.take()).is_none() && !ui.get_settings_open() {
            return;
        }
        ui.set_settings_open(false);
        ui.set_settings_fields(ModelRc::default());
    }

    /// Open `config.toml` for a setting the page cannot edit.
    pub(super) fn settings_open_config(&self) {
        let path = &self.engine.paths().config;
        let opened = crate::config::ensure_config_file_at(path)
            .map_err(|e| e.to_string())
            .and_then(|()| crate::core::process::open(&path.to_string_lossy()));
        match opened {
            Ok(()) => self.hide(),
            Err(e) => {
                if let Some(ui) = self.weak.upgrade() {
                    ui.set_settings_status(format!("Cannot open config.toml: {e}").into());
                    ui.set_settings_status_error(true);
                }
            }
        }
    }
}

/// Rows for a manifest and effective values, as the page would show them (screenshots, tests).
#[cfg(test)]
pub(super) fn demo_rows(manifest: &str, values: &serde_json::Value) -> Vec<SettingField> {
    let m = crate::plugin::manifest::Manifest::parse(manifest, "demo").unwrap();
    m.settings.iter().map(|s| FieldState::new(&m.id, s, values.get(&s.key)).row()).collect()
}

/// The rows of entry `name` of table setting `key`, as its form would show them (screenshots).
#[cfg(test)]
pub(super) fn demo_entry_rows(manifest: &str, values: &serde_json::Value, key: &str, name: &str) -> Vec<SettingField> {
    let m = crate::plugin::manifest::Manifest::parse(manifest, "demo").unwrap();
    let s = m.settings.iter().find(|s| s.key == key).unwrap();
    let parent = FieldState::new(&m.id, s, values.get(key));
    EntryEdit::new(&m.id, 0, &parent, name).fields.iter().map(FieldState::row).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setting(toml_text: &str) -> Setting {
        let m = crate::plugin::manifest::Manifest::parse(
            &format!("id = \"p\"\nname = \"P\"\nversion = \"1\"\n[[settings]]\n{toml_text}"),
            "test",
        )
        .unwrap();
        m.settings[0].clone()
    }

    #[test]
    fn kinds_follow_the_setting_type() {
        let cases = [
            ("key = \"a\"\ntype = \"string\"", kind::TEXT),
            ("key = \"a\"\ntype = \"number\"\ndefault = 3", kind::NUMBER),
            ("key = \"a\"\ntype = \"bool\"", kind::BOOL),
            ("key = \"a\"\ntype = \"choice\"\noptions = [\"x\", \"y\"]", kind::CHOICE),
            ("key = \"a\"\ntype = \"secret\"", kind::SECRET),
            ("key = \"a\"\ntype = \"secrets\"", kind::SECRETS),
            ("key = \"a\"\ntype = \"list\"\ndefault = [\"~\"]", kind::LINES),
            ("key = \"a\"\ntype = \"list\"\ndefault = []\nexample = \"[[plugins.p.a]]\\nk = 1\"", kind::TOML),
            ("key = \"a\"\ntype = \"table\"", kind::PAIRS),
            ("key = \"a\"\ntype = \"table\"\ndefault = { x = { y = 1 } }", kind::TOML),
            ("key = \"a\"\ntype = \"table\"\nexample = \"[plugins.p.a.x]\"", kind::TOML),
            ("key = \"a\"\ntype = \"table\"\n[[settings.fields]]\nkey = \"b\"\ntype = \"string\"", kind::ENTRIES),
            ("key = \"a\"\ntype = \"color\"", kind::READ_ONLY),
        ];
        for (text, want) in cases {
            assert_eq!(FieldState::new("p", &setting(text), None).kind, want, "{text}");
        }
    }

    #[test]
    fn secret_values_never_reach_the_page() {
        let s = setting("key = \"api_key\"\ntype = \"secret\"");
        let f = FieldState::new("p", &s, Some(&serde_json::json!("sk-123")));
        let row = f.row();
        assert!(row.is_set);
        assert_eq!(row.text, "");
        let s = setting("key = \"tokens\"\ntype = \"secrets\"");
        let f = FieldState::new("p", &s, Some(&serde_json::json!({"gh": "tok-1"})));
        let row = f.row();
        assert_eq!(row.names.iter().collect::<Vec<_>>(), vec![SharedString::from("gh")]);
        assert!(!format!("{row:?}").contains("tok-1"));
    }

    #[test]
    fn changes_are_only_what_was_edited() {
        let s = setting("key = \"model\"\ntype = \"string\"\ndefault = \"\"");
        let mut f = FieldState::new("p", &s, Some(&serde_json::json!("a")));
        assert!(f.changes().is_empty());
        f.text = "b".into();
        assert_eq!(f.changes(), vec![SettingChange::Set { key: "model".into(), text: "b".into() }]);
        f.reset = true;
        assert_eq!(f.changes(), vec![SettingChange::Reset { key: "model".into() }]);

        let s = setting("key = \"api_key\"\ntype = \"secret\"");
        let mut f = FieldState::new("p", &s, Some(&serde_json::json!("old")));
        assert!(f.changes().is_empty(), "an untouched secret is kept");
        f.text = "new".into();
        assert_eq!(f.changes(), vec![SettingChange::Set { key: "api_key".into(), text: "new".into() }]);
        f.text.clear();
        f.reset = true;
        assert_eq!(f.changes(), vec![SettingChange::Reset { key: "api_key".into() }]);
    }

    #[test]
    fn lists_show_one_item_per_line_and_tables_as_toml() {
        let s = setting("key = \"roots\"\ntype = \"list\"\ndefault = [\"~\"]");
        let f = FieldState::new("p", &s, Some(&serde_json::json!(["~", "/data"])));
        assert_eq!(f.text, "~\n/data");
        let s = setting("key = \"engines\"\ntype = \"table\"\ndefault = {}");
        let f = FieldState::new("p", &s, Some(&serde_json::json!({"g": "https://g/?q={q}"})));
        assert_eq!(f.text, "g = \"https://g/?q={q}\"");
    }

    const SERVERS: &str = "key = \"servers\"\ntype = \"table\"\ndefault = {}\n\
        [[settings.fields]]\nkey = \"command\"\ntype = \"list\"\ndefault = []\n\
        [[settings.fields]]\nkey = \"url\"\ntype = \"string\"\n\
        [[settings.fields]]\nkey = \"env\"\ntype = \"table\"\n\
        [[settings.fields]]\nkey = \"allow\"\ntype = \"list\"\ndefault = []";

    fn servers(value: &serde_json::Value) -> FieldState {
        FieldState::new("p", &setting(SERVERS), Some(value))
    }

    #[test]
    fn tables_with_fields_list_entries_and_string_tables_are_pairs() {
        let f = servers(&serde_json::json!({"fs": {"command": ["npx", "srv"]}, "gh": {"url": "https://h"}}));
        assert_eq!(f.kind, kind::ENTRIES);
        let row = f.row();
        assert_eq!(row.names.iter().collect::<Vec<_>>(), ["fs", "gh"]);
        assert_eq!(row.values.iter().collect::<Vec<_>>(), ["npx srv", "https://h"]);
        assert!(!f.dirty());

        let env = &setting(SERVERS).fields[2];
        let mut p = FieldState::new("p", env, Some(&serde_json::json!({"A": "b"})));
        assert_eq!(p.kind, kind::PAIRS);
        let mut t = p.table();
        t.insert("C".into(), toml::Value::String("d e".into()));
        p.set_table(&t);
        assert!(p.dirty());
        assert_eq!(p.row().values.iter().collect::<Vec<_>>(), ["b", "d e"]);
        assert_eq!(p.value().unwrap(), Some(toml::Value::Table(t)));
        let mixed = FieldState::new("p", env, Some(&serde_json::json!({"A": 1})));
        assert_eq!(mixed.kind, kind::TOML);
    }

    #[test]
    fn pasted_json_adds_entries() {
        let mut f = FieldState::new(
            "p",
            &setting(&SERVERS.replacen("default = {}", "default = {}\nimport = \"mcp\"", 1)),
            Some(&serde_json::json!({"fs": {"command": ["old"]}, "keep": {"url": "https://k"}})),
        );
        assert_eq!(f.placeholder, "name of a new entry, or paste JSON");
        let raw = r#"{"mcpServers": {"fs": {"command": "npx", "args": ["srv"]}, "gh": {"type": "http", "url": "https://h"}}}"#;
        assert!(f.setting.imports(raw) && !f.setting.imports("fs"));
        assert_eq!(f.import(raw).unwrap(), ["fs", "gh"]);
        let t = f.table();
        assert_eq!(t["fs"]["command"], toml::Value::try_from(["npx", "srv"]).unwrap(), "replaced");
        assert_eq!(t["gh"].as_table().unwrap().keys().collect::<Vec<_>>(), ["url"], "type dropped");
        assert!(t.contains_key("keep") && f.dirty());
        assert!(f.import("{oops").is_err());
    }

    #[test]
    fn an_entry_form_writes_its_fields_into_the_table() {
        let mut v = SettingsView {
            plugin: "p".into(),
            title: "P settings".into(),
            fields: vec![servers(&serde_json::json!({
                "fs": {"command": ["x"], "url": "", "extra": 1},
                "gh": {"url": "https://h"},
            }))],
            editing: None,
            warned: false,
            saving: false,
        };
        v.editing = Some(EntryEdit::new("p", 0, &v.fields[0], "fs"));
        assert_eq!(v.page_title(), "P settings › fs");
        let e = v.editing.as_mut().unwrap();
        assert_eq!(e.fields.iter().map(|f| f.text.as_str()).collect::<Vec<_>>(), ["fs", "x", "", "", ""]);
        e.fields[0].text = "files".into();
        e.fields[1].text = "npx\n-y\nsrv\n".into();
        e.fields[4].text = "read_file".into();
        v.apply_entry().unwrap();
        assert!(v.editing.is_none());
        let t = v.fields[0].table();
        assert!(!t.contains_key("fs"), "renamed");
        let want: toml::Table =
            toml::from_str("command = [\"npx\", \"-y\", \"srv\"]\nallow = [\"read_file\"]\nextra = 1").unwrap();
        assert_eq!(t["files"].as_table(), Some(&want), "empty url left out, unknown key kept");
        assert!(v.fields[0].dirty());
        let changes = v.fields[0].changes();
        let [SettingChange::Set { text, .. }] = changes.as_slice() else { panic!("{changes:?}") };
        assert!(v.fields[0].setting.parse_input(text).is_ok());

        // A new entry: the name must be free.
        v.editing = Some(EntryEdit::new("p", 0, &v.fields[0], "gh2"));
        let e = v.editing.as_mut().unwrap();
        assert!(e.original.is_none());
        e.fields[0].text = "gh".into();
        assert!(v.apply_entry().is_err());
        let e = v.editing.as_mut().unwrap();
        assert_eq!(e.fields[0].error, "gh exists already");
        e.fields[0].text = " ".into();
        assert!(v.apply_entry().is_err());
        let e = v.editing.as_mut().unwrap();
        e.fields[0].text = "local".into();
        e.fields[2].text = "http://localhost:1/mcp".into();
        v.apply_entry().unwrap();
        assert_eq!(v.fields[0].table()["local"]["url"].as_str(), Some("http://localhost:1/mcp"));
    }

    #[test]
    fn the_ai_plugin_lists_its_mcp_servers() {
        let m = crate::plugin::manifest::Manifest::parse(include_str!("../../plugins/ai/plugin.toml"), "ai").unwrap();
        let s = m.settings.iter().find(|s| s.key == "mcp_servers").unwrap();
        let f = FieldState::new("ai", s, Some(&serde_json::json!({})));
        assert_eq!(f.kind, kind::ENTRIES);
        let e = EntryEdit::new("ai", 0, &f, "files");
        let kinds: Vec<i32> = e.fields.iter().map(|f| f.kind).collect();
        assert_eq!(kinds, [kind::TEXT, kind::LINES, kind::TEXT, kind::PAIRS, kind::PAIRS, kind::LINES]);
    }
}
