//! Adding a reference voice, as screens 2b to 2d of the design.
//!
//! The layout follows one decision: **reading is the task, so it gets the
//! space.** The script is a large left panel set in reading type with a lock on
//! its label — because the words are given, the panel must not look like a
//! field you could type in. Everything to do with the microphone — device,
//! level, button, timer — sits in a narrow right column.
//!
//! Reference audio and reference text stay one object: the script *is* the
//! transcript, known by construction, which is what keeps speech recognition
//! out of the enrolment path entirely.
//!
//! One recorder serves three entry points — setup step 3, the sidebar, and the
//! empty composer — so it renders either as a full screen during setup or as a
//! sheet over the workspace afterwards.

use gpui::prelude::FluentBuilder;
use gpui::*;
use gpui_component::{ActiveTheme, Sizable, StyledExt};
use rust_i18n::t;

use crate::icon;
use crate::recorder::ENROLMENT_SCRIPT;
use crate::theme;
use crate::ui;
use crate::{Enrolment, VoiceStudio};

/// Sheet geometry from 2b: 900 wide, 14px radius, over a 38% scrim.
const SHEET_WIDTH: f32 = 900.0;
/// The recorder column. Fixed, so the script keeps every pixel the window gains.
const RECORDER_WIDTH: f32 = 300.0;
/// Below this the two columns stop fitting side by side and stack instead.
const STACK_BELOW: f32 = 760.0;
/// What the script takes to read aloud, used for the place indicator and the
/// timer's target. The script is fixed, so this is a property of it.
const SCRIPT_SECONDS: f32 = 20.0;
/// Bars in the level meter, as the design draws it.
const METER_BARS: usize = 18;

/// Where a wizard step stands relative to the one being shown.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Step {
    Done,
    Active,
    Upcoming,
}

/// One row of the quality report: a state, and what it means in plain words.
enum Check {
    Pass(String),
    Warn(String),
    /// Neither good nor bad yet — something that can only be judged once the
    /// take is finished, said now so its absence is not read as a failure.
    Pending(String),
}

/// The script as separate sentences, which is how the design sets it — one
/// block per sentence, so the eye can find its place again after a glance away.
fn script_sentences() -> Vec<String> {
    ENROLMENT_SCRIPT
        .split_inclusive(['.', '!', '?'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect()
}

impl VoiceStudio {
    /// Measured from the design: 22px circle, 15px glyph, 12.5px label.
    /// Measured from the design: 22px circle, 15px glyph, 12.5px label. Three
    /// states, not two — a step still to come is outlined, so the wizard says
    /// how far along it is rather than showing everything as current.
    fn step_dot(index: u8, label: &str, state: Step) -> AnyElement {
        div()
            .h_flex()
            .gap(px(8.0))
            .items_center()
            .child(
                div()
                    .size(px(22.0))
                    .flex_none()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .map(|d| match state {
                        Step::Done => d.bg(theme::hex(0xE9F5EF)),
                        Step::Active => d.bg(theme::hex(0x1F1C19)),
                        Step::Upcoming => d.border_1().border_color(theme::hex(0xD8D0C4)),
                    })
                    .child(match state {
                        Step::Done => {
                            icon::icon(icon::name::CHECK, 15.0, theme::hex(0x1B5C41))
                                .into_any_element()
                        }
                        // The step number is set in mono, per the design.
                        _ => div()
                            .font_family(theme::FONT_MONO)
                            .text_size(px(11.0))
                            .font_semibold()
                            .text_color(match state {
                                Step::Active => theme::hex(0xFFF9F2),
                                _ => theme::hex(0x857D72),
                            })
                            .child(index.to_string())
                            .into_any_element(),
                    }),
            )
            .child(
                div()
                    .text_size(px(12.5))
                    .map(|d| match state {
                        Step::Done => d.font_medium().text_color(theme::hex(0x5F594F)),
                        Step::Active => d.font_semibold().text_color(theme::hex(0x171717)),
                        Step::Upcoming => d.font_medium().text_color(theme::hex(0x857D72)),
                    })
                    .child(label.to_string()),
            )
            .into_any_element()
    }

    pub(crate) fn stepper(&self, active: u8, _cx: &Context<Self>) -> impl IntoElement {
        // 12px between steps, a 44x1 rule between them.
        let rule = || div().w(px(44.0)).h(px(1.0)).flex_none().bg(theme::hex(theme::RULE));
        let steps = [
            (1u8, t!("setup.step_runtime").to_string()),
            (2, t!("setup.step_model").to_string()),
            (3, t!("setup.step_voice").to_string()),
        ];
        div()
            .h_flex()
            .gap(px(12.0))
            .items_center()
            .children(steps.iter().enumerate().flat_map(|(i, (index, label))| {
                let dot = Self::step_dot(
                    *index,
                    label,
                    match (*index).cmp(&active) {
                        std::cmp::Ordering::Less => Step::Done,
                        std::cmp::Ordering::Equal => Step::Active,
                        std::cmp::Ordering::Greater => Step::Upcoming,
                    },
                );
                if i == 0 {
                    vec![dot]
                } else {
                    vec![rule().into_any_element(), dot]
                }
            }))
    }

    fn check_row(&self, check: &Check, cx: &Context<Self>) -> AnyElement {
        let (glyph, colour, text) = match check {
            Check::Pass(t) => (icon::name::CHECK, cx.theme().success, t.clone()),
            Check::Warn(t) => (icon::name::WARNING, cx.theme().warning, t.clone()),
            Check::Pending(t) => (icon::name::GRAPHIC_EQ, theme::hex(0x857D72), t.clone()),
        };
        div()
            .h_flex()
            .w_full()
            .gap(px(9.0))
            .items_start()
            .child(icon::icon(glyph, 17.0, colour))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.0))
                    .text_size(px(12.0))
                    .line_height(px(17.0))
                    .text_color(theme::hex(0x5F594F))
                    .child(text),
            )
            .into_any_element()
    }

    /// Both left panels share this frame: the reading column, padded as the
    /// design pads it.
    fn left_column() -> Div {
        div().v_flex().flex_1().min_w(px(0.0)).p(px(20.0)).gap(px(14.0))
    }

    /// The label above the words. The lock says they are given, not typed —
    /// without it a bordered box full of text reads as somewhere to type.
    fn locked_label(label: String, note: Option<(String, bool)>) -> Div {
        div()
            .h_flex()
            .w_full()
            .items_center()
            .gap(px(8.0))
            .child(icon::icon(icon::name::LOCK, 15.0, theme::hex(0x857D72)))
            .child(ui::section_label(label.to_uppercase()))
            .child(div().flex_1())
            .when_some(note, |d, (note, live)| {
                d.child(
                    div()
                        .text_size(px(11.5))
                        .text_color(if live {
                            theme::hex(0x8F4406)
                        } else {
                            theme::hex(0x6B645A)
                        })
                        .child(note),
                )
            })
    }

    /// Left panel while reading: the words, in reading type. During a take the
    /// label carries your place in the script instead of its length, which is
    /// the one thing worth knowing while your eyes are on the text.
    fn script_panel(&self) -> Div {
        let sentences = script_sentences();
        let recording = matches!(self.enrolment, Enrolment::Recording);

        // Roughly where the reader is, from elapsed time against the whole.
        // Approximate by construction — it is a place-keeper, not a cursor.
        let elapsed = self.recorder.as_ref().map(|r| r.elapsed_seconds()).unwrap_or(0.0);
        let at = ((elapsed / SCRIPT_SECONDS * sentences.len() as f32).floor() as usize + 1)
            .min(sentences.len());

        let note = if recording {
            t!("enrol.sentence_of", at = at, of = sentences.len()).to_string()
        } else {
            t!(
                "enrol.script_length",
                chars = ENROLMENT_SCRIPT.chars().count(),
                seconds = SCRIPT_SECONDS as u32
            )
            .to_string()
        };

        Self::left_column()
            .child(Self::locked_label(
                t!("enrol.read_this_script").to_string(),
                Some((note, recording)),
            ))
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .gap(px(12.0))
                    .px(px(20.0))
                    .py(px(18.0))
                    .bg(theme::surface(false))
                    .border_1()
                    .border_color(theme::hex(0xEBE4D9))
                    .rounded(px(12.0))
                    .shadow_sm()
                    .children(sentences.iter().enumerate().map(|(i, sentence)| {
                        let live = recording && i + 1 == at;
                        let read = recording && i + 1 < at;
                        div()
                            .h_flex()
                            .items_stretch()
                            .gap(px(12.0))
                            // A rule marks the sentence being read; text already
                            // read steps further back than text still to come,
                            // so the eye lands forward rather than backward.
                            .when(live, |d| {
                                d.child(
                                    div()
                                        .w(px(3.0))
                                        .flex_none()
                                        .rounded(px(2.0))
                                        .bg(theme::hex(0xFF8A1F)),
                                )
                            })
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.0))
                                    .text_size(px(17.0))
                                    .line_height(px(29.0))
                                    .text_color(if read {
                                        theme::hex(0xB0A79B)
                                    } else if recording && !live {
                                        theme::hex(0x5F594F)
                                    } else {
                                        theme::hex(0x171717)
                                    })
                                    .child(sentence.clone()),
                            )
                    })),
            )
            .child(
                div()
                    .w_full()
                    .text_size(px(12.0))
                    .line_height(px(19.0))
                    .text_color(theme::hex(0x6B645A))
                    .child(
                        if recording { t!("enrol.keep_reading") } else { t!("enrol.same_script") }
                            .to_string(),
                    ),
            )
    }

    /// The take, with something to play it. Reviewing a recording you cannot
    /// hear is not reviewing it, so this is the first thing in the panel.
    fn playback_card(&self, cx: &mut Context<Self>) -> Div {
        let (playing, progress) = self.take_playback();
        let (levels, seconds) = match self.take.as_ref() {
            Some(take) => (take.levels.clone(), take.seconds),
            None => (Vec::new(), 0.0),
        };

        ui::card()
            .w_full()
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .items_center()
                    .gap(px(14.0))
                    .px(px(18.0))
                    .py(px(16.0))
                    .child(
                        ui::play_button(playing, false)
                            .id("play-take")
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_take(cx))),
                    )
                    .child(ui::waveform(
                        &levels,
                        progress,
                        48.0,
                        theme::hex(0xFF8A1F),
                        theme::hex(0xFFB264),
                    ))
                    .child(
                        ui::mono(crate::workspace::duration(seconds), 12.0, theme::hex(0x6B645A))
                            .flex_none(),
                    ),
            )
    }

    /// Left panel once there is a take: hear it, then read what it will be
    /// saved against. The script is set smaller here — it is no longer
    /// something to read aloud, only something to check against.
    fn review_panel(&self, cx: &mut Context<Self>) -> Div {
        Self::left_column()
            .child(self.playback_card(cx))
            .child(Self::locked_label(t!("enrol.saved_as_reference").to_string(), None))
            .child(
                div()
                    .w_full()
                    .px(px(16.0))
                    .py(px(14.0))
                    .rounded(px(12.0))
                    .bg(theme::hex(0xFFFDFA))
                    .border_1()
                    .border_color(theme::hex(0xEBE4D9))
                    .text_size(px(13.5))
                    .line_height(px(22.0))
                    .text_color(theme::hex(0x5F594F))
                    .child(ENROLMENT_SCRIPT),
            )
    }

    /// Which microphone, asked once before the take and hidden during it —
    /// changing device mid-recording is not a thing that can happen.
    fn mic_picker(&self) -> Div {
        div()
            .v_flex()
            .w_full()
            .gap(px(7.0))
            .child(ui::section_label(t!("enrol.mic_label").to_string().to_uppercase()))
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .h(px(36.0))
                    .items_center()
                    .justify_between()
                    .pl(px(12.0))
                    .pr(px(10.0))
                    .rounded(px(8.0))
                    .bg(theme::surface(false))
                    .border_1()
                    .border_color(theme::hex(0xD8D0C4))
                    .text_size(px(12.5))
                    .child(t!("enrol.default_device").to_string())
                    .child(icon::icon(icon::name::EXPAND_MORE, 18.0, theme::hex(0x857D72))),
            )
    }

    /// The meter is the proof the microphone is live. Silence with a dead meter
    /// and silence with a live one look identical otherwise.
    fn meter_row(&self, recording: bool) -> Div {
        let levels = match self.recorder.as_ref() {
            Some(rec) if recording => rec.meter(METER_BARS),
            _ => vec![0.0; METER_BARS],
        };
        let loudest = levels.iter().cloned().fold(0.0f32, f32::max);

        div()
            .h_flex()
            .w_full()
            .h(px(34.0))
            .items_center()
            .gap(px(10.0))
            .child(
                div()
                    .h_flex()
                    .flex_none()
                    .h_full()
                    .items_center()
                    .gap(px(2.0))
                    .children(levels.iter().map(|level| {
                        div()
                            .w(px(3.0))
                            .h(px(6.0 + level * 26.0))
                            .flex_none()
                            .rounded(px(2.0))
                            .bg(if !recording {
                                theme::hex(0xE4DCD0)
                            } else if *level > 0.08 {
                                theme::hex(0xFF8A1F)
                            } else {
                                theme::hex(0xFFCB93)
                            })
                    })),
            )
            .child(div().flex_1())
            .child(
                ui::mono(
                    if !recording {
                        t!("enrol.quiet").to_string()
                    } else if loudest > 0.08 {
                        t!("enrol.good_level").to_string()
                    } else {
                        t!("enrol.too_quiet").to_string()
                    },
                    11.5,
                    if recording && loudest > 0.08 {
                        theme::hex(0x287A57)
                    } else {
                        theme::hex(0x857D72)
                    },
                )
                .flex_none(),
            )
    }

    /// How long the take is running, against how long the script takes to read.
    fn timer_row(&self) -> Div {
        let elapsed = self.recorder.as_ref().map(|r| r.elapsed_seconds()).unwrap_or(0.0);
        div()
            .h_flex()
            .w_full()
            .items_baseline()
            .gap(px(8.0))
            .child(
                ui::mono(crate::workspace::duration(elapsed), 24.0, theme::hex(0x171717))
                    .font_semibold(),
            )
            .child(ui::mono(
                t!("enrol.of_about", seconds = SCRIPT_SECONDS as u32).to_string(),
                12.0,
                theme::hex(0x857D72),
            ))
    }

    /// The one big button: start in accent orange, stop outlined in red. The
    /// colour change is the state — a label alone is missed mid-sentence.
    fn record_button(&self, recording: bool, cx: &mut Context<Self>) -> AnyElement {
        div()
            .h_flex()
            .w_full()
            .h(px(44.0))
            .flex_none()
            .gap(px(9.0))
            .items_center()
            .justify_center()
            .rounded(px(10.0))
            .text_size(px(14.0))
            .font_semibold()
            .when(!recording, |d| d.bg(theme::hex(0xFF6E08)).text_color(theme::hex(0xFFFEFD)))
            .when(recording, |d| {
                d.bg(theme::surface(false))
                    .border_2()
                    .border_color(theme::hex(0xC7362B))
                    .text_color(theme::hex(0xC7362B))
            })
            .child(icon::icon(
                if recording { icon::name::STOP_CIRCLE } else { icon::name::MIC },
                19.0,
                if recording { theme::hex(0xC7362B) } else { theme::hex(0xFFFEFD) },
            ))
            .child(if recording {
                t!("enrol.stop").to_string()
            } else {
                t!("enrol.start").to_string()
            })
            .id("rec-toggle")
            .on_click(cx.listener(|this, _, _, cx| {
                if matches!(this.enrolment, Enrolment::Recording) {
                    this.stop_recording(cx)
                } else {
                    this.start_recording(cx)
                }
            }))
            .into_any_element()
    }

    /// A block of checks under its own rule, which is where the design puts
    /// everything the app has to say about the take.
    fn checks_block(&self, checks: Vec<Check>, cx: &Context<Self>) -> Div {
        div()
            .v_flex()
            .w_full()
            .gap(px(8.0))
            .pt(px(12.0))
            .border_t_1()
            .border_color(theme::hex(0xEBE4D9))
            .children(checks.iter().map(|c| self.check_row(c, cx)))
    }

    /// Right column, before a take: device, level, and the two ways to get a
    /// recording — make one, or bring one.
    fn recorder_ready(&self, cx: &mut Context<Self>) -> Div {
        Self::recorder_column()
            .child(self.mic_picker())
            .child(self.meter_row(false))
            .child(self.record_button(false, cx))
            .child(
                ui::secondary_button(
                    Some((icon::name::UPLOAD_FILE, 0x5F594F)),
                    t!("enrol.upload").to_string(),
                )
                .h(px(36.0))
                .w_full()
                .justify_center()
                .id("import-reference")
                .on_click(cx.listener(|this, _, window, cx| this.import_reference(window, cx))),
            )
            // What the app cannot check for you. Nothing transcribes the file,
            // so a recording of different words would be paired with the wrong
            // text and every clip made from it would slur.
            .child(
                div()
                    .text_size(px(11.5))
                    .line_height(px(17.0))
                    .text_color(theme::hex(0x857D72))
                    .child(t!("enrol.upload_detail").to_string()),
            )
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .gap(px(7.0))
                    .pt(px(12.0))
                    .border_t_1()
                    .border_color(theme::hex(0xEBE4D9))
                    .child(
                        div()
                            .text_size(px(12.5))
                            .font_semibold()
                            .child(t!("enrol.tips_title").to_string()),
                    )
                    .children(
                        [
                            t!("enrol.tip_1").to_string(),
                            t!("enrol.tip_2").to_string(),
                            t!("enrol.tip_3").to_string(),
                        ]
                        .map(|tip| {
                            div()
                                .text_size(px(12.0))
                                .line_height(px(18.0))
                                .text_color(theme::hex(0x5F594F))
                                .child(tip)
                        }),
                    ),
            )
    }

    /// Right column during a take. The device selector is gone — there is
    /// nothing to decide now — and the way out of a bad take is stated.
    fn recorder_recording(&self, cx: &mut Context<Self>) -> Div {
        Self::recorder_column()
            .child(self.timer_row())
            .child(self.meter_row(true))
            .child(self.record_button(true, cx))
            .child(
                ui::secondary_button(None, t!("enrol.discard").to_string())
                    .h(px(36.0))
                    .w_full()
                    .justify_center()
                    .id("discard-take")
                    .on_click(cx.listener(|this, _, _, cx| this.discard_take(cx))),
            )
            .child(self.checks_block(
                vec![
                    Check::Pass(t!("enrol.mic_reaching").to_string()),
                    Check::Pass(t!("enrol.no_clipping_yet").to_string()),
                    Check::Pending(t!("enrol.room_noise").to_string()),
                ],
                cx,
            ))
    }

    /// Right column once there is a take: what it is worth, what it will be
    /// called, and the two ways forward. Naming and saving live here rather
    /// than in a bar of their own — this is the only state that can save.
    fn recorder_review(&self, quality: &crate::recorder::Quality, cx: &mut Context<Self>) -> Div {
        let agreed = self.consent_given;
        Self::recorder_column()
            .child(self.checks_block(
                vec![
                    Check::Pass(
                        t!(
                            "enrol.whole_script",
                            time = crate::workspace::duration(quality.seconds)
                        )
                        .to_string(),
                    ),
                    Check::Pass(t!("enrol.no_clipping").to_string()),
                    Check::Warn(
                        t!("enrol.snr", db = format!("{:.0}", quality.snr_db)).to_string(),
                    ),
                ],
                cx,
            ))
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .gap(px(7.0))
                    .child(ui::section_label(t!("enrol.name").to_string().to_uppercase()))
                    .child(gpui_component::input::Input::new(&self.voice_name).small()),
            )
            .child(div().flex_1())
            // Not in the design, kept deliberately: a cloned voice is a
            // likeness, and consent to make one is given per voice rather than
            // remembered from a previous enrolment.
            .child(
                ui::secondary_button(
                    Some((
                        if agreed { icon::name::CHECK } else { icon::name::RADIO_UNCHECKED },
                        if agreed { 0x287A57 } else { 0x857D72 },
                    )),
                    t!("enrol.consent_ask").to_string(),
                )
                .h(px(36.0))
                .w_full()
                .justify_center()
                .id("consent")
                .on_click(cx.listener(|this, _, _, cx| {
                    this.consent_given = !this.consent_given;
                    cx.notify();
                })),
            )
            .child(
                ui::secondary_button(
                    Some((icon::name::MIC, 0xC7362B)),
                    t!("enrol.again").to_string(),
                )
                .h(px(36.0))
                .w_full()
                .justify_center()
                .id("record-again")
                .on_click(cx.listener(|this, _, _, cx| this.discard_take(cx))),
            )
            .child(
                div()
                    .h_flex()
                    .w_full()
                    .h(px(40.0))
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .rounded(px(8.0))
                    .bg(if agreed { theme::hex(0xFF6E08) } else { theme::hex(0xFFCB93) })
                    .text_size(px(13.5))
                    .font_semibold()
                    .text_color(theme::hex(0xFFFEFD))
                    .id("save-voice")
                    .on_click(cx.listener(|this, _, _, cx| this.confirm_enrolment(cx)))
                    .child(t!("enrol.save").to_string()),
            )
    }

    /// The frame every right-column state shares.
    fn recorder_column() -> Div {
        div()
            .v_flex()
            .w(px(RECORDER_WIDTH))
            .flex_none()
            .p(px(20.0))
            .gap(px(14.0))
            .bg(theme::hex(0xFFFDFA))
            .border_l_1()
            .border_color(theme::hex(0xEBE4D9))
    }

    /// Right column: the microphone and nothing else — device, level, the
    /// button, and what the take is worth once there is one.
    fn recorder_panel(&self, cx: &mut Context<Self>) -> Div {
        match &self.enrolment {
            Enrolment::Recording => self.recorder_recording(cx),
            Enrolment::Review(quality) => self.recorder_review(&quality.clone(), cx),
            Enrolment::Rejected(reason) => Self::recorder_column()
                .child(self.mic_picker())
                .child(self.meter_row(false))
                .child(self.record_button(false, cx))
                .child(self.checks_block(vec![Check::Warn(reason.clone())], cx)),
            _ => self.recorder_ready(cx),
        }
    }

    /// The header: what to do now, and why the words matter.
    fn enrolment_header(&self, closable: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let (title, detail) = match &self.enrolment {
            Enrolment::Recording => (t!("enrol.title"), t!("enrol.recording_detail")),
            Enrolment::Review(_) if self.imported.is_some() => {
                (t!("enrol.check_title"), t!("enrol.imported_detail"))
            }
            Enrolment::Review(_) => (t!("enrol.check_title"), t!("enrol.check_detail")),
            // Opened from a clip's panel, the sheet says what saving will do
            // to that clip; opened during setup it introduces the idea.
            _ if !self.in_setup => (t!("enrol.record_title"), t!("enrol.record_subtitle")),
            _ => (t!("enrol.title"), t!("enrol.subtitle")),
        };
        div()
            .h_flex()
            .w_full()
            .items_center()
            .gap(px(12.0))
            .px(px(20.0))
            .py(px(16.0))
            .border_b_1()
            .border_color(theme::hex(0xEBE4D9))
            .child(
                div()
                    .v_flex()
                    .flex_1()
                    .min_w(px(0.0))
                    .child(
                        div()
                            .font_family(theme::FONT_DISPLAY)
                            .text_size(px(17.0))
                            .font_semibold()
                            .child(title.to_string()),
                    )
                    .child(
                        div()
                            .text_size(px(12.5))
                            .line_height(px(19.0))
                            .text_color(theme::hex(0x5F594F))
                            .mt(px(3.0))
                            .child(detail.to_string()),
                    ),
            )
            .when(closable, |d| {
                d.child(
                    div()
                        .flex_none()
                        .child(icon::icon(icon::name::CLOSE, 20.0, theme::hex(0x5F594F)))
                        .id("close-enrolment")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel_enrolment(cx))),
                )
            })
            // During setup there is no window behind this to close to, but
            // there is a way past it: the bundled voice works with every model,
            // so recording is an offer here and not a toll.
            .when(!closable, |d| {
                d.child(
                    ui::secondary_button(None, t!("enrol.skip").to_string())
                        .h(px(32.0))
                        .px(px(12.0))
                        .text_size(px(12.0))
                        .id("skip-enrolment")
                        .on_click(cx.listener(|this, _, _, cx| this.finish_setup(cx))),
                )
            })
    }

    /// The two columns, stacked when the pane is too narrow to read them side
    /// by side.
    fn enrolment_columns(&self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let stacked = window.viewport_size().width < px(STACK_BELOW + 120.0);
        div()
            .w_full()
            .when(stacked, |d| d.v_flex())
            .when(!stacked, |d| d.h_flex())
            // After the direction, never before: `h_flex` sets `items_center`,
            // which would centre the shorter column and leave a gap above it.
            .items_stretch()
            .child(match self.enrolment {
                Enrolment::Review(_) => self.review_panel(cx),
                _ => self.script_panel(),
            })
            .child(self.recorder_panel(cx))
    }

    /// The recorder's body, identical wherever it is shown.
    fn enrolment_panel(&self, closable: bool, window: &Window, cx: &mut Context<Self>) -> Div {
        div()
            .v_flex()
            .w_full()
            .bg(cx.theme().background)
            .border_1()
            .border_color(theme::hex(0xE4DCD0))
            .rounded(px(14.0))
            .overflow_hidden()
            .child(self.enrolment_header(closable, cx))
            .child(self.enrolment_columns(window, cx))
    }

    /// Setup step 3: the recorder as a full screen, under the wizard's stepper.
    pub(crate) fn enrolment_screen(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .v_flex()
            .flex_1()
            .min_h(px(0.0))
            .items_center()
            .bg(cx.theme().background)
            .pt(px(30.0))
            .px(px(40.0))
            .child(
                div()
                    .v_flex()
                    .w_full()
                    .max_w(px(SHEET_WIDTH))
                    .gap(px(18.0))
                    .child(self.stepper(3, cx))
                    // Not closable during setup: there is no workspace behind
                    // it yet to close back to.
                    .child(self.enrolment_panel(false, window, cx)),
            )
    }

    /// The same recorder, later: a sheet over the workspace, because adding a
    /// second voice is not a reason to leave what you were doing.
    pub(crate) fn enrolment_sheet(
        &mut self,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .absolute()
            .inset_0()
            .bg(gpui::rgba(0x1F1C1961))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .w(px(SHEET_WIDTH))
                    .max_w(relative(0.94))
                    .shadow_lg()
                    .child(self.enrolment_panel(true, window, cx)),
            )
            .into_any_element()
    }
}
