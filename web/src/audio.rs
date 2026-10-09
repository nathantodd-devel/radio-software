//! Playing the scanner's audio through the browser.

use std::cell::{Cell, RefCell};

use js_sys::{Float32Array, Function, Reflect};
use scanner_proto::AUDIO_RATE;
use wasm_bindgen::JsCast;
use web_sys::{AudioContext, AudioContextOptions};

/// How far ahead of the clock audio is scheduled when a transmission
/// starts, to ride out uneven arrival over the network.
const LEAD_SECS: f64 = 0.15;
/// If more than this is queued the browser has fallen behind, and playback
/// skips ahead rather than lagging for good.
const MAX_QUEUED_SECS: f64 = 1.0;

/// The page's audio output. Off until [`start`](Self::start)ed, which
/// browsers only allow once the page has been clicked.
#[derive(Default)]
pub struct Audio {
    context: RefCell<Option<AudioContext>>,
    /// When, on the context's clock, the audio queued so far runs out.
    queued_until: Cell<f64>,
}

impl Audio {
    pub fn listening(&self) -> bool {
        self.context.borrow().is_some()
    }

    /// Start playing what arrives. False if the browser wouldn't.
    pub fn start(&self) -> bool {
        let options = AudioContextOptions::new();
        options.set_sample_rate(AUDIO_RATE as f32);
        let context = AudioContext::new_with_context_options(&options).ok();
        if let Some(context) = &context {
            // Browsers start it suspended unless a click is being handled.
            let _ = context.resume();
        }
        self.queued_until.set(0.0);
        *self.context.borrow_mut() = context;
        self.listening()
    }

    pub fn stop(&self) {
        if let Some(context) = self.context.borrow_mut().take() {
            let _ = context.close();
        }
    }

    /// Queue a piece of audio: little-endian 16-bit mono samples.
    pub fn play(&self, pcm: &[u8]) {
        let context = self.context.borrow();
        let (Some(context), true) = (context.as_ref(), pcm.len() >= 2) else {
            return;
        };
        let samples: Vec<f32> = pcm
            .chunks_exact(2)
            .map(|s| f32::from(i16::from_le_bytes([s[0], s[1]])) / 32768.0)
            .collect();
        // Web Audio won't read from this program's memory, which is shared
        // between threads, so the samples go through an array of the page's.
        let array = Float32Array::new_with_length(samples.len() as u32);
        array.copy_from(&samples);
        let Ok(buffer) = context.create_buffer(1, samples.len() as u32, AUDIO_RATE as f32) else {
            return;
        };
        let copied = Reflect::get(&buffer, &"copyToChannel".into())
            .ok()
            .and_then(|f| f.dyn_into::<Function>().ok())
            .and_then(|f| f.call2(&buffer, &array, &0.into()).ok());
        let (Some(_), Ok(source)) = (copied, context.create_buffer_source()) else {
            return;
        };
        source.set_buffer(Some(&buffer));
        if source.connect_with_audio_node(&context.destination()).is_err() {
            return;
        }
        // Pieces play end to end. After a gap (a new transmission), or if
        // too much has built up, start again a little ahead of now.
        let now = context.current_time();
        let mut at = self.queued_until.get();
        if at < now || at > now + MAX_QUEUED_SECS {
            at = now + LEAD_SECS;
        }
        if source.start_with_when(at).is_ok() {
            self.queued_until.set(at + samples.len() as f64 / f64::from(AUDIO_RATE));
        }
    }
}
