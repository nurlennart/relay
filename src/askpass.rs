//! Answers OpenSSH's prompts (passwords, passphrases, one-time codes, unknown host keys) in
//! Relay's window for ssh processes that run without a terminal.
//!
//! Relay names its own executable as `SSH_ASKPASS`. Started that way, it is a helper: it
//! connects back to the Relay window over loopback, proves it was started for that ssh
//! process with a random secret that only reaches it through the environment, and prints
//! the answer for ssh. Answers never touch the disk.
use serde::{Deserialize, Serialize};
use std::{
    io::{BufRead, BufReader, Write},
    net::TcpStream,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    sync::{mpsc, oneshot},
};

/// Where the helper finds the window: `127.0.0.1:port secret`.
pub const ENV: &str = "RELAY_ASKPASS";

/// The longest request line a helper may send.
const MAX_REQUEST: u64 = 64 * 1024;

/// What ssh wants from the person at the window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Kind {
    /// A password, passphrase, or code, typed without showing it.
    Secret,
    /// A yes/no question answered with text, such as accepting an unknown host key.
    YesNo,
    /// A yes/no question answered by the helper's exit status, such as agent confirmation.
    Confirm,
    /// Information with nothing to answer, such as "touch your security key". ssh ends the
    /// helper when it is done.
    Notice,
}

impl Kind {
    /// The kind of prompt, from ssh's `SSH_ASKPASS_PROMPT` hint and the prompt's text.
    pub fn of(hint: &str, text: &str) -> Self {
        match hint {
            "confirm" => Self::Confirm,
            "none" => Self::Notice,
            _ if text.contains("(yes/no") => Self::YesNo,
            _ => Self::Secret,
        }
    }
}

/// A question from ssh, waiting for an answer from the window.
pub struct Request {
    pub id: u64,
    pub text: String,
    pub kind: Kind,
    /// Sends the answer; `None` declines. Notices never need one.
    pub reply: oneshot::Sender<Option<String>>,
}

/// What the window hears from its helpers.
pub enum Message {
    Ask(Request),
    /// The helper for request `id` went away before it was answered, usually because ssh
    /// finished with it (notices) or ended.
    Gone(u64),
}

#[derive(Serialize, Deserialize)]
struct Question {
    secret: String,
    text: String,
    hint: String,
}

#[derive(Serialize, Deserialize)]
struct Answer {
    answer: Option<String>,
}

/// Runs Relay as ssh's askpass helper when ssh started it that way, returning its exit code.
pub fn helper() -> Option<i32> {
    let endpoint = std::env::var(ENV).ok()?;
    let text = std::env::args().nth(1).unwrap_or_default();
    let hint = std::env::var("SSH_ASKPASS_PROMPT").unwrap_or_default();
    Some(match ask(&endpoint, &text, &hint) {
        Ok(Some(answer)) => {
            let mut out = std::io::stdout().lock();
            match writeln!(out, "{answer}").and_then(|_| out.flush()) {
                Ok(()) => 0,
                Err(_) => 1,
            }
        }
        Ok(None) | Err(_) => 1,
    })
}

/// Asks the window at `endpoint` and waits for its answer.
fn ask(endpoint: &str, text: &str, hint: &str) -> Result<Option<String>, String> {
    let (address, secret) = endpoint.split_once(' ').ok_or("malformed endpoint")?;
    let address: std::net::SocketAddr = address.parse().map_err(|_| "malformed endpoint")?;
    if !address.ip().is_loopback() {
        return Err("the endpoint is not on this computer".into());
    }
    let mut stream =
        TcpStream::connect_timeout(&address, Duration::from_secs(5)).map_err(|e| e.to_string())?;
    let question = Question {
        secret: secret.into(),
        text: text.into(),
        hint: hint.into(),
    };
    let mut line = serde_json::to_vec(&question).map_err(|e| e.to_string())?;
    line.push(b'\n');
    stream.write_all(&line).map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(stream)
        .read_line(&mut reply)
        .map_err(|e| e.to_string())?;
    if reply.is_empty() {
        return Ok(None);
    }
    let reply: Answer = serde_json::from_str(&reply).map_err(|e| e.to_string())?;
    // ssh reads one line, so an answer cannot carry line breaks.
    Ok(reply.answer.filter(|a| !a.contains(['\n', '\r'])))
}

/// A loopback listener for one ssh process's helpers.
pub struct Listener {
    listener: TcpListener,
    secret: String,
}

impl Listener {
    pub async fn bind() -> Result<Self, String> {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .map_err(|e| format!("Cannot prepare password prompts: {e}"))?;
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| format!("Cannot prepare password prompts: {e}"))?;
        let secret = bytes.iter().map(|b| format!("{b:02x}")).collect();
        Ok(Self { listener, secret })
    }

    /// The value of [`ENV`] for the ssh process.
    pub fn endpoint(&self) -> Result<String, String> {
        let address = self.listener.local_addr().map_err(|e| e.to_string())?;
        Ok(format!("{address} {}", self.secret))
    }

    /// Accepts helpers until `messages` closes, passing their questions on.
    pub async fn serve(self, messages: mpsc::UnboundedSender<Message>) {
        let mut next = 0u64;
        loop {
            let accepted = tokio::select! {
                accepted = self.listener.accept() => accepted,
                _ = messages.closed() => return,
            };
            let Ok((stream, _)) = accepted else {
                continue;
            };
            next += 1;
            tokio::spawn(answer(stream, next, self.secret.clone(), messages.clone()));
        }
    }
}

/// Serves one helper: checks its secret, passes the question on, and returns the answer.
async fn answer(
    stream: tokio::net::TcpStream,
    id: u64,
    secret: String,
    messages: mpsc::UnboundedSender<Message>,
) {
    let (read, mut write) = stream.into_split();
    let mut read = tokio::io::BufReader::new(read.take(MAX_REQUEST));
    let mut line = String::new();
    let request = tokio::time::timeout(Duration::from_secs(10), read.read_line(&mut line));
    if !matches!(request.await, Ok(Ok(n)) if n > 0) {
        return;
    }
    let Ok(question) = serde_json::from_str::<Question>(&line) else {
        return;
    };
    if !same(question.secret.as_bytes(), secret.as_bytes()) {
        return;
    }
    let (reply, answered) = oneshot::channel();
    let kind = Kind::of(&question.hint, &question.text);
    let text = question.text.trim_end().to_string();
    if messages
        .send(Message::Ask(Request {
            id,
            text,
            kind,
            reply,
        }))
        .is_err()
    {
        return;
    }
    // The helper sends nothing more, so a finished read means it went away.
    let mut rest = [0u8; 1];
    let answer = tokio::select! {
        answer = answered => answer.ok().flatten(),
        _ = read.read(&mut rest) => {
            let _ = messages.send(Message::Gone(id));
            return;
        }
    };
    let mut reply = serde_json::to_vec(&Answer { answer }).unwrap_or_default();
    reply.push(b'\n');
    let _ = write.write_all(&reply).await;
    let _ = write.shutdown().await;
}

/// Compares secrets in time that does not depend on where they differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompts_are_recognized() {
        let host = "The authenticity of host 'build (192.0.2.1)' can't be established.\n\
                    ED25519 key fingerprint is SHA256:abc.\n\
                    Are you sure you want to continue connecting (yes/no/[fingerprint])? ";
        assert_eq!(Kind::of("", host), Kind::YesNo);
        assert_eq!(Kind::of("", "deploy@build's password: "), Kind::Secret);
        assert_eq!(Kind::of("confirm", "Allow use of key?"), Kind::Confirm);
        assert_eq!(Kind::of("none", "Confirm user presence"), Kind::Notice);
        assert!(same(b"abc", b"abc") && !same(b"abc", b"abd") && !same(b"abc", b"ab"));
    }

    /// Runs a listener on its own thread, answering each question with `answer`.
    fn window(answer: Option<&'static str>) -> (String, std::sync::mpsc::Receiver<String>) {
        let (seen, questions) = std::sync::mpsc::channel();
        let (endpoint, ready) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = Listener::bind().await.unwrap();
                endpoint.send(listener.endpoint().unwrap()).unwrap();
                let (messages, mut incoming) = mpsc::unbounded_channel();
                tokio::spawn(listener.serve(messages));
                while let Some(message) = incoming.recv().await {
                    if let Message::Ask(request) = message {
                        seen.send(request.text).unwrap();
                        let _ = request.reply.send(answer.map(String::from));
                    }
                }
            });
        });
        (ready.recv().unwrap(), questions)
    }

    #[test]
    fn helpers_get_answers_from_the_window() {
        let (endpoint, questions) = window(Some("hunter2"));
        assert_eq!(
            ask(&endpoint, "Password: ", "").unwrap().as_deref(),
            Some("hunter2")
        );
        assert_eq!(questions.recv().unwrap(), "Password:");
    }

    #[test]
    fn declining_and_wrong_secrets_answer_nothing() {
        let (endpoint, questions) = window(None);
        assert_eq!(ask(&endpoint, "Password: ", "").unwrap(), None);
        assert_eq!(questions.recv().unwrap(), "Password:");
        let (address, _) = endpoint.split_once(' ').unwrap();
        let forged = format!("{address} {}", "0".repeat(64));
        assert_eq!(ask(&forged, "Password: ", "").unwrap(), None);
        assert!(
            questions.recv_timeout(Duration::from_millis(300)).is_err(),
            "a helper without the secret reached the window"
        );
        assert!(ask("192.0.2.1:22 secret", "Password: ", "").is_err());
    }
}
