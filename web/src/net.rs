//! The connection to the scanner: a WebSocket back to wherever the page was
//! served from.

use std::rc::Rc;

use futures::channel::mpsc::UnboundedSender;
use js_sys::{ArrayBuffer, Uint8Array};
use scanner_proto::{ClientMessage, SOCKET_PATH, ServerMessage};
use wasm_bindgen::prelude::*;
use web_sys::{BinaryType, MessageEvent, WebSocket};

use crate::audio::Audio;

/// What the connection hands the user interface.
pub enum Incoming {
    Message(Box<ServerMessage>),
    /// The connection was lost, or couldn't be made.
    Closed,
}

/// An open connection; dropping it closes it.
pub struct Connection {
    socket: WebSocket,
    _on_message: Closure<dyn FnMut(MessageEvent)>,
    _on_close: Closure<dyn FnMut(JsValue)>,
}

impl Connection {
    /// Connect to the server. Messages go to `incoming`; audio goes straight
    /// to `audio`, since nothing on screen depends on it.
    pub fn open(incoming: UnboundedSender<Incoming>, audio: Rc<Audio>) -> Option<Self> {
        let location = web_sys::window()?.location();
        let scheme = if location.protocol().ok()? == "https:" {
            "wss"
        } else {
            "ws"
        };
        let socket = WebSocket::new(&format!("{scheme}://{}{SOCKET_PATH}", location.host().ok()?)).ok()?;
        socket.set_binary_type(BinaryType::Arraybuffer);

        let on_message = Closure::<dyn FnMut(MessageEvent)>::new({
            let incoming = incoming.clone();
            move |event: MessageEvent| {
                let data = event.data();
                if let Some(json) = data.as_string() {
                    // Anything unrecognised is from a newer server: ignore it.
                    if let Ok(message) = serde_json::from_str(&json) {
                        incoming.unbounded_send(Incoming::Message(Box::new(message))).ok();
                    }
                } else if let Ok(buffer) = data.dyn_into::<ArrayBuffer>() {
                    audio.play(&Uint8Array::new(&buffer).to_vec());
                }
            }
        });
        let on_close = Closure::<dyn FnMut(JsValue)>::new(move |_| {
            incoming.unbounded_send(Incoming::Closed).ok();
        });
        socket.set_onmessage(Some(on_message.as_ref().unchecked_ref()));
        socket.set_onclose(Some(on_close.as_ref().unchecked_ref()));
        Some(Self {
            socket,
            _on_message: on_message,
            _on_close: on_close,
        })
    }

    pub fn send(&self, message: &ClientMessage) {
        self.socket.send_with_str(&message.to_json()).ok();
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // The callbacks are about to be freed: the browser mustn't call them.
        self.socket.set_onmessage(None);
        self.socket.set_onclose(None);
        self.socket.close().ok();
    }
}
