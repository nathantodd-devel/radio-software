//! Editing the channel database from the app: forms for a system and for a
//! channel, shown in place of the channel list.

use gpui::{AnyElement, App, Context, KeyDownEvent, Window, div, prelude::*, px, rgb};
use scanner::plan::{Entry, Kind, Plan};

use crate::theme::theme;
use crate::{ScannerView, button, section_title};

/// Frequencies the Airspy can tune, in MHz.
const TUNABLE_MHZ: std::ops::RangeInclusive<f64> = 24.0..=1800.0;
const FIELD_WIDTH: f32 = 420.;

/// Apply a key press to a line of text being typed: characters are added
/// at the end, Backspace removes the last, and Ctrl+V pastes. Returns
/// whether the key was one of those.
pub fn type_into(text: &mut String, event: &KeyDownEvent, cx: &App) -> bool {
    let keystroke = &event.keystroke;
    let modifiers = keystroke.modifiers;
    if keystroke.key == "backspace" {
        text.pop();
    } else if keystroke.key == "v" && (modifiers.control || modifiers.platform) {
        let pasted = cx
            .read_from_clipboard()
            .and_then(|item| item.text())
            .unwrap_or_default();
        text.extend(pasted.chars().filter(|c| !c.is_control()));
    } else if let Some(typed) = keystroke
        .key_char
        .as_ref()
        .filter(|_| !(modifiers.control || modifiers.platform || modifiers.alt))
    {
        text.push_str(typed);
    } else {
        return false;
    }
    true
}

/// What kind of channel is being edited.
#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    FmWide,
    FmNarrow,
    P25,
    Talkgroup,
}

impl Mode {
    const ALL: [(Mode, &'static str); 4] = [
        (Mode::FmWide, "FM wide"),
        (Mode::FmNarrow, "FM narrow"),
        (Mode::P25, "P25 channel"),
        (Mode::Talkgroup, "P25 talkgroup"),
    ];
}

pub enum Editing {
    /// A system; `None` for one that doesn't exist yet.
    System(Option<i64>),
    /// A channel of `system`; the entry holds what the form doesn't show.
    Channel { system: i64, entry: Entry, mode: Mode },
}

struct Field {
    label: &'static str,
    hint: &'static str,
    value: String,
}

/// A form that is open.
pub struct Editor {
    what: Editing,
    fields: Vec<Field>,
    /// The field being typed into.
    active: usize,
    /// Delete was clicked once; a second click does it.
    confirm_delete: bool,
    error: Option<String>,
}

// Fields of the system form.
const NAME: usize = 0;
const LOCATION: usize = 1;
const SITE_FREQUENCIES: usize = 2;
// Fields of the channel form (NAME is shared).
const DESCRIPTION: usize = 1;
const FREQUENCY: usize = 2;
const TONE: usize = 3;
const NAC: usize = 4;
const TALKGROUP: usize = 5;

impl Editor {
    fn field(label: &'static str, hint: &'static str, value: String) -> Field {
        Field { label, hint, value }
    }

    /// The fields that apply to what is being edited.
    fn visible(&self) -> Vec<usize> {
        match &self.what {
            Editing::System(_) => vec![NAME, LOCATION, SITE_FREQUENCIES],
            Editing::Channel { mode, .. } => match mode {
                Mode::FmWide | Mode::FmNarrow => vec![NAME, DESCRIPTION, FREQUENCY, TONE],
                Mode::P25 => vec![NAME, DESCRIPTION, FREQUENCY, NAC],
                Mode::Talkgroup => vec![NAME, DESCRIPTION, TALKGROUP],
            },
        }
    }

    fn text(&self, field: usize) -> &str {
        self.fields[field].value.trim()
    }

    /// The channel the form describes, or what is wrong with it.
    fn channel(&self, entry: &Entry, mode: Mode) -> Result<Entry, String> {
        let freq_hz = || -> Result<f64, String> {
            let mhz: f64 = self
                .text(FREQUENCY)
                .parse()
                .map_err(|_| "Frequency must be a number of MHz, like 154.34")?;
            if !TUNABLE_MHZ.contains(&mhz) {
                return Err(format!(
                    "Frequency must be between {} and {} MHz",
                    TUNABLE_MHZ.start(),
                    TUNABLE_MHZ.end()
                ));
            }
            Ok((mhz * 1e6).round())
        };
        let optional = |field: usize| Some(self.text(field)).filter(|t| !t.is_empty());
        let kind = match mode {
            Mode::FmWide | Mode::FmNarrow => Kind::Analog {
                freq_hz: freq_hz()?,
                tone_hz: optional(TONE)
                    .map(|t| {
                        t.parse()
                            .map_err(|_| "Tone must be a CTCSS frequency in Hz, like 114.8, or empty")
                    })
                    .transpose()?,
                narrow: mode == Mode::FmNarrow,
            },
            Mode::P25 => Kind::Digital {
                freq_hz: freq_hz()?,
                nac: optional(NAC)
                    .map(|n| u16::from_str_radix(n, 16).map_err(|_| "NAC must be three hex digits, like 293, or empty"))
                    .transpose()?,
            },
            Mode::Talkgroup => Kind::Talkgroup {
                system: 0,
                id: self
                    .text(TALKGROUP)
                    .parse()
                    .map_err(|_| "Talkgroup must be a number from 0 to 65535")?,
            },
        };
        if self.text(NAME).is_empty() {
            return Err("The channel needs a name".into());
        }
        Ok(Entry {
            channel_id: entry.channel_id,
            skip: entry.skip,
            priority: entry.priority,
            record: entry.record,
            kind,
            tag: self.text(NAME).to_string(),
            desc: self.text(DESCRIPTION).to_string(),
        })
    }

    /// The system form's site frequencies in Hz, or what is wrong with them.
    fn site_frequencies(&self) -> Result<Vec<f64>, String> {
        let mut freqs = Vec::new();
        for part in self.text(SITE_FREQUENCIES).split([',', ' ']).filter(|p| !p.is_empty()) {
            let mhz: f64 = part
                .parse()
                .map_err(|_| format!("{part:?} is not a frequency in MHz"))?;
            if !TUNABLE_MHZ.contains(&mhz) {
                return Err(format!("{part} MHz is outside what the Airspy can tune"));
            }
            freqs.push((mhz * 1e6).round());
        }
        Ok(freqs)
    }
}

impl ScannerView {
    /// Open the form for a system, or for a new one.
    pub fn edit_system(&mut self, id: Option<i64>, window: &mut Window) {
        let system = id.and_then(|id| self.systems.iter().find(|s| s.id == id));
        let sites = id
            .and_then(|id| self.db.as_ref()?.p25_frequencies(id).ok())
            .unwrap_or_default();
        let sites: Vec<String> = sites.iter().map(|hz| format!("{}", hz / 1e6)).collect();
        self.open_editor(
            Editor {
                what: Editing::System(id),
                fields: vec![
                    Editor::field(
                        "Name",
                        "City Police",
                        system.map(|s| s.name.clone()).unwrap_or_default(),
                    ),
                    Editor::field(
                        "Location",
                        "Shown as the heading it is listed under",
                        system.map(|s| s.location.clone()).unwrap_or_default(),
                    ),
                    Editor::field(
                        "P25 trunked site frequencies",
                        "MHz, separated by commas; only for a trunked system with talkgroups",
                        sites.join(", "),
                    ),
                ],
                active: NAME,
                confirm_delete: false,
                error: None,
            },
            window,
        );
    }

    /// Open the form for a channel, or for a new one in `system`.
    pub fn edit_channel(&mut self, system: i64, id: Option<i64>, window: &mut Window) {
        let existing = id.and_then(|id| self.db.as_ref()?.channel(id).ok());
        let (system, entry) = existing.unwrap_or((
            system,
            Entry {
                channel_id: None,
                skip: false,
                priority: false,
                record: false,
                kind: Kind::Analog {
                    freq_hz: 0.0,
                    tone_hz: None,
                    narrow: true,
                },
                tag: String::new(),
                desc: String::new(),
            },
        ));
        let mhz = |hz: f64| {
            if hz > 0.0 {
                format!("{}", hz / 1e6)
            } else {
                String::new()
            }
        };
        let (mode, freq, tone, nac, talkgroup) = match entry.kind {
            Kind::Analog {
                freq_hz,
                tone_hz,
                narrow,
            } => (
                if narrow { Mode::FmNarrow } else { Mode::FmWide },
                mhz(freq_hz),
                tone_hz.map(|t| t.to_string()).unwrap_or_default(),
                String::new(),
                String::new(),
            ),
            Kind::Digital { freq_hz, nac } => (
                Mode::P25,
                mhz(freq_hz),
                String::new(),
                nac.map(|n| format!("{n:03X}")).unwrap_or_default(),
                String::new(),
            ),
            Kind::Talkgroup { id, .. } => (
                Mode::Talkgroup,
                String::new(),
                String::new(),
                String::new(),
                id.to_string(),
            ),
            Kind::OtherTalkgroups { .. } => return,
        };
        self.open_editor(
            Editor {
                fields: vec![
                    Editor::field("Name", "Short name shown in the list", entry.tag.clone()),
                    Editor::field("Description", "Optional", entry.desc.clone()),
                    Editor::field("Frequency", "MHz, like 154.34", freq),
                    Editor::field("CTCSS tone", "Hz, like 114.8; empty to open on any signal", tone),
                    Editor::field("NAC", "Three hex digits, like 293; empty for any", nac),
                    Editor::field("Talkgroup", "Decimal number, like 865", talkgroup),
                ],
                what: Editing::Channel { system, entry, mode },
                active: NAME,
                confirm_delete: false,
                error: None,
            },
            window,
        );
    }

    fn open_editor(&mut self, editor: Editor, window: &mut Window) {
        self.editor = Some(editor);
        self.show_settings = false;
        window.focus(&self.editor_focus);
    }

    /// Typing in the open form: Tab moves between fields, Enter saves and
    /// Escape closes without saving.
    fn editor_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(editor) = &mut self.editor else { return };
        let visible = editor.visible();
        let at = visible.iter().position(|&f| f == editor.active).unwrap_or(0);
        match event.keystroke.key.as_str() {
            "escape" => self.editor = None,
            "enter" => self.save_editor(),
            "tab" => {
                let step = if event.keystroke.modifiers.shift {
                    visible.len() - 1
                } else {
                    1
                };
                editor.active = visible[(at + step) % visible.len()];
            }
            _ => {
                let active = editor.active;
                if !type_into(&mut editor.fields[active].value, event, cx) {
                    return;
                }
                editor.confirm_delete = false;
            }
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Write the form to the database and rescan with the result. On a
    /// problem the form stays open and says what it is.
    fn save_editor(&mut self) {
        let (Some(editor), Some(db)) = (&self.editor, &mut self.db) else {
            return;
        };
        let saved = match &editor.what {
            Editing::System(id) => (|| {
                if editor.text(NAME).is_empty() {
                    return Err("The system needs a name".to_string());
                }
                let sites = editor.site_frequencies()?;
                let id = match id {
                    Some(id) => {
                        db.rename_system(*id, editor.text(NAME), editor.text(LOCATION))?;
                        *id
                    }
                    None => db.add_system(editor.text(NAME), editor.text(LOCATION), &Plan::default())?,
                };
                db.set_p25_frequencies(id, &sites)
            })(),
            Editing::Channel { system, entry, mode } => editor
                .channel(entry, *mode)
                .and_then(|channel| db.save_channel(*system, &channel).map(|_| ())),
        };
        match saved {
            Ok(()) => self.close_editor(),
            Err(e) => self.editor.as_mut().unwrap().error = Some(e),
        }
    }

    /// Delete what the form is editing; the first call only asks.
    fn delete_edited(&mut self) {
        let (Some(editor), Some(db)) = (&mut self.editor, &mut self.db) else {
            return;
        };
        if !editor.confirm_delete {
            editor.confirm_delete = true;
            return;
        }
        let deleted = match &editor.what {
            Editing::System(Some(id)) => db.remove_system(*id),
            Editing::Channel { entry, .. } => entry.channel_id.map_or(Ok(()), |id| db.delete_channel(id)),
            Editing::System(None) => Ok(()),
        };
        match deleted {
            Ok(()) => self.close_editor(),
            Err(e) => editor.error = Some(e),
        }
    }

    /// Move the channel being edited up or down its system's list.
    fn move_edited(&mut self, up: bool) {
        let (Some(editor), Some(db)) = (&mut self.editor, &mut self.db) else {
            return;
        };
        if let Editing::Channel { entry, .. } = &editor.what
            && let Some(id) = entry.channel_id
        {
            editor.error = db.move_channel(id, up).err();
            self.rescan();
        }
    }

    /// Close the form and scan with whatever changed.
    fn close_editor(&mut self) {
        self.editor = None;
        self.rescan();
    }

    fn rescan(&mut self) {
        self.reload_systems();
        self.remember_selection();
        self.start();
    }

    pub fn editor_page(&self, editor: &Editor, cx: &mut Context<Self>) -> AnyElement {
        let (title, exists) = match &editor.what {
            Editing::System(id) => (if id.is_some() { "Edit system" } else { "New system" }, id.is_some()),
            Editing::Channel { entry, .. } => (
                if entry.channel_id.is_some() {
                    "Edit channel"
                } else {
                    "New channel"
                },
                entry.channel_id.is_some(),
            ),
        };
        let mut page = div()
            .id("editor")
            .track_focus(&self.editor_focus)
            .on_key_down(cx.listener(|this, event, _, cx| this.editor_key(event, cx)))
            .flex_1()
            .h_full()
            .overflow_y_scroll()
            .child(section_title(title));

        if let Editing::Channel { mode, .. } = &editor.what {
            let current = *mode;
            page = page.child(
                div()
                    .flex()
                    .gap_1()
                    .px_4()
                    .py_2()
                    .children(Mode::ALL.iter().map(|&(mode, label)| {
                        button(label, label, mode == current, theme().accent).on_click(cx.listener(
                            move |this, _, _, cx| {
                                if let Some(editor) = &mut this.editor
                                    && let Editing::Channel { mode: chosen, .. } = &mut editor.what
                                {
                                    *chosen = mode;
                                    editor.active = NAME;
                                }
                                cx.notify();
                            },
                        ))
                    })),
            );
        }

        for field in editor.visible() {
            let Field { label, hint, value } = &editor.fields[field];
            let active = field == editor.active;
            page = page.child(
                div()
                    .px_4()
                    .py_1()
                    .child(div().text_sm().text_color(rgb(theme().muted)).child(*label))
                    .child(
                        div()
                            .id(("field", field))
                            .w(px(FIELD_WIDTH))
                            .px_2()
                            .py_1()
                            .rounded_md()
                            .border_1()
                            .border_color(rgb(if active { theme().accent } else { theme().border }))
                            .bg(rgb(theme().panel))
                            .cursor_text()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                if let Some(editor) = &mut this.editor {
                                    editor.active = field;
                                }
                                window.focus(&this.editor_focus);
                                cx.notify();
                            }))
                            .child(match (value.is_empty(), active) {
                                (true, false) => div().text_color(rgb(theme().muted)).child(*hint),
                                (_, true) => div().child(format!("{value}▏")),
                                (false, false) => div().child(value.clone()),
                            }),
                    )
                    .when(active && !value.is_empty(), |field| {
                        field.child(div().text_xs().text_color(rgb(theme().muted)).child(*hint))
                    }),
            );
        }

        if let Some(error) = &editor.error {
            page = page.child(
                div()
                    .px_4()
                    .py_2()
                    .text_sm()
                    .text_color(rgb(theme().error))
                    .child(error.clone()),
            );
        }

        let mut actions = div()
            .flex()
            .gap_2()
            .px_4()
            .py_3()
            .child(
                button("save", "Save", true, theme().accent).on_click(cx.listener(|this, _, _, cx| {
                    this.save_editor();
                    cx.notify();
                })),
            )
            .child(
                button("cancel", "Cancel", false, theme().accent).on_click(cx.listener(|this, _, _, cx| {
                    this.editor = None;
                    cx.notify();
                })),
            );
        if exists {
            if matches!(editor.what, Editing::Channel { .. }) {
                for (id, label, up) in [("move-up", "Move up", true), ("move-down", "Move down", false)] {
                    actions = actions.child(button(id, label, false, theme().accent).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.move_edited(up);
                            cx.notify();
                        },
                    )));
                }
            }
            if let Editing::System(Some(system)) = editor.what {
                actions = actions.child(button("add-channel", "Add channel", false, theme().accent).on_click(
                    cx.listener(move |this, _, window, cx| {
                        this.edit_channel(system, None, window);
                        cx.notify();
                    }),
                ));
            }
            let label = if editor.confirm_delete {
                "Click again to delete"
            } else {
                "Delete"
            };
            actions = actions.child(div().flex_1()).child(
                button("delete", label, editor.confirm_delete, theme().error).on_click(cx.listener(
                    |this, _, _, cx| {
                        this.delete_edited();
                        cx.notify();
                    },
                )),
            );
        }
        page.child(actions)
            .child(
                div()
                    .px_4()
                    .text_sm()
                    .text_color(rgb(theme().muted))
                    .child(match editor.what {
                        Editing::System(_) => "Tab moves between fields, Enter saves, Escape cancels.",
                        Editing::Channel { .. } => {
                            "Tab moves between fields, Enter saves, Escape cancels. \
                         Channels higher in the list win when several are active."
                        }
                    }),
            )
            .into_any_element()
    }
}
