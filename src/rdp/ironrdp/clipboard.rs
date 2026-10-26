//! Clipboard text in both directions over the CLIPRDR channel. Text copied remotely is fetched
//! right away and handed to the UI; text copied locally is offered and sent when pasted.
use ironrdp_cliprdr::{
    backend::{ClipboardMessage, CliprdrBackend},
    pdu::{
        ClipboardFormat, ClipboardFormatId, ClipboardGeneralCapabilityFlags, FileContentsRequest,
        FileContentsResponse, FormatDataRequest, FormatDataResponse, LockDataId,
        OwnedFormatDataResponse,
    },
};
use std::sync::{Arc, Mutex};

/// Where the backend reports, from inside the connection's session loop.
pub trait Sink: std::fmt::Debug + Send {
    /// A message for the channel, sent on the next turn of the session loop.
    fn send(&self, message: ClipboardMessage);
    /// Text the remote computer copied.
    fn copied(&self, text: String);
}

#[derive(Debug)]
pub struct Backend<S> {
    sink: S,
    /// The local text offered to the remote computer, if any.
    offered: Arc<Mutex<Option<String>>>,
}

impl<S: Sink> Backend<S> {
    pub fn new(sink: S, offered: Arc<Mutex<Option<String>>>) -> Self {
        Self { sink, offered }
    }

    fn offered(&self) -> Option<String> {
        self.offered.lock().ok().and_then(|text| text.clone())
    }
}

impl<S: Sink + 'static> ironrdp_core::AsAny for Backend<S> {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// The formats Relay offers: Unicode text only.
pub fn text_formats() -> Vec<ClipboardFormat> {
    vec![ClipboardFormat::new(ClipboardFormatId::CF_UNICODETEXT)]
}

impl<S: Sink + 'static> CliprdrBackend for Backend<S> {
    fn temporary_directory(&self) -> &str {
        // Only used for file transfers, which Relay does not offer.
        ".cliprdr"
    }

    fn client_capabilities(&self) -> ClipboardGeneralCapabilityFlags {
        ClipboardGeneralCapabilityFlags::USE_LONG_FORMAT_NAMES
    }

    fn on_ready(&mut self) {}

    fn on_request_format_list(&mut self) {
        // Re-offer what was copied before the channel was ready.
        if self.offered().is_some() {
            self.sink
                .send(ClipboardMessage::SendInitiateCopy(text_formats()));
        }
    }

    fn on_process_negotiated_capabilities(&mut self, _: ClipboardGeneralCapabilityFlags) {}

    fn on_remote_copy(&mut self, formats: &[ClipboardFormat]) {
        if formats
            .iter()
            .any(|f| f.id == ClipboardFormatId::CF_UNICODETEXT)
        {
            self.sink.send(ClipboardMessage::SendInitiatePaste(
                ClipboardFormatId::CF_UNICODETEXT,
            ));
        }
    }

    fn on_format_data_request(&mut self, request: FormatDataRequest) {
        let response = match self.offered() {
            Some(text) if request.format == ClipboardFormatId::CF_UNICODETEXT => {
                OwnedFormatDataResponse::new_unicode_string(&text)
            }
            _ => OwnedFormatDataResponse::new_error(),
        };
        self.sink.send(ClipboardMessage::SendFormatData(response));
    }

    fn on_format_data_response(&mut self, response: FormatDataResponse<'_>) {
        if !response.is_error()
            && let Ok(text) = response.to_unicode_string()
        {
            self.sink.copied(text);
        }
    }

    fn on_file_contents_request(&mut self, _: FileContentsRequest) {}

    fn on_file_contents_response(&mut self, _: FileContentsResponse<'_>) {}

    fn on_lock(&mut self, _: LockDataId) {}

    fn on_unlock(&mut self, _: LockDataId) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, Default, Clone)]
    struct Recorder {
        sent: Arc<Mutex<Vec<String>>>,
        copied: Arc<Mutex<Vec<String>>>,
    }

    impl Sink for Recorder {
        fn send(&self, message: ClipboardMessage) {
            self.sent.lock().unwrap().push(format!("{message:?}"));
        }
        fn copied(&self, text: String) {
            self.copied.lock().unwrap().push(text);
        }
    }

    #[test]
    fn remote_text_is_fetched_and_local_text_is_served() {
        let recorder = Recorder::default();
        let offered = Arc::new(Mutex::new(None));
        let mut backend = Backend::new(recorder.clone(), offered.clone());

        backend.on_request_format_list();
        backend.on_remote_copy(&[ClipboardFormat::new(ClipboardFormatId::new(49_000))]);
        assert!(
            recorder.sent.lock().unwrap().is_empty(),
            "nothing to offer or fetch"
        );

        backend.on_remote_copy(&text_formats());
        backend.on_format_data_response(FormatDataResponse::new_unicode_string("héllo"));
        assert_eq!(*recorder.copied.lock().unwrap(), ["héllo"]);

        backend.on_format_data_request(FormatDataRequest {
            format: ClipboardFormatId::CF_UNICODETEXT,
        });
        *offered.lock().unwrap() = Some("local".into());
        backend.on_format_data_request(FormatDataRequest {
            format: ClipboardFormatId::CF_UNICODETEXT,
        });
        let sent = recorder.sent.lock().unwrap();
        assert_eq!(sent.len(), 3);
        assert!(sent[0].contains("SendInitiatePaste"));
        assert!(sent[1].contains("is_error: true"), "nothing offered yet");
        assert!(sent[2].contains("is_error: false"));
    }
}
