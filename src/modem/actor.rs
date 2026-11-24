//! Serialised AT command engine. One task owns the serial stream; commands
//! are queued through a bounded channel and executed strictly one at a time,
//! while unsolicited result codes are forwarded on a separate channel.

use super::parser::{classify_final, expects_pdu_line, parse_urc, Final, LineBuffer, Urc};
use super::transport::BoxStream;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};
use tokio::time::Instant;

#[derive(Debug, Clone)]
pub struct AtCommand {
    pub text: String,
    pub timeout: Duration,
    /// Data sent after the `>` prompt, terminated with Ctrl-Z (PDU submission).
    pub payload: Option<String>,
    /// Never write the command text to logs (PIN entry).
    pub sensitive: bool,
}

impl AtCommand {
    pub fn new(text: impl Into<String>, timeout: Duration) -> Self {
        Self {
            text: text.into(),
            timeout,
            payload: None,
            sensitive: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AtError {
    /// The modem answered with an error result code.
    #[error("modem returned {0}")]
    Rejected(String),
    /// No final result arrived in time. `ambiguous` is true when data had
    /// already been handed to the modem, so it may still have acted on it.
    #[error("command timed out")]
    Timeout { ambiguous: bool },
    #[error("modem connection closed")]
    Closed,
}

pub type AtResult = Result<Vec<String>, AtError>;

type Request = (AtCommand, oneshot::Sender<AtResult>);

#[derive(Clone)]
pub struct AtHandle {
    tx: mpsc::Sender<Request>,
}

impl AtHandle {
    /// Queue a command and wait for its final result.
    pub async fn run(&self, cmd: AtCommand) -> AtResult {
        let (tx, rx) = oneshot::channel();
        self.tx.send((cmd, tx)).await.map_err(|_| AtError::Closed)?;
        rx.await.map_err(|_| AtError::Closed)?
    }

    pub async fn at(&self, text: &str, timeout: Duration) -> AtResult {
        self.run(AtCommand::new(text, timeout)).await
    }
}

enum Phase {
    /// Waiting for the final result of a plain command.
    Response,
    /// Waiting for the `>` prompt before sending the payload.
    Prompt,
    /// Payload sent; waiting for the final result.
    Submitted,
}

struct Active {
    cmd: AtCommand,
    reply: oneshot::Sender<AtResult>,
    lines: Vec<String>,
    deadline: Instant,
    phase: Phase,
}

/// How long to wait for the data prompt before aborting a submission.
const PROMPT_TIMEOUT: Duration = Duration::from_secs(8);

/// Start the engine on a stream. The URC receiver closes when the stream fails.
pub fn spawn(stream: BoxStream, modem_id: String) -> (AtHandle, mpsc::Receiver<Urc>) {
    let (tx, rx) = mpsc::channel::<Request>(32);
    let (urc_tx, urc_rx) = mpsc::channel::<Urc>(64);
    tokio::spawn(run(stream, rx, urc_tx, modem_id));
    (AtHandle { tx }, urc_rx)
}

async fn run(
    stream: BoxStream,
    mut rx: mpsc::Receiver<Request>,
    urc_tx: mpsc::Sender<Urc>,
    modem_id: String,
) {
    let (mut reader, mut writer) = tokio::io::split(stream);
    let mut buf = LineBuffer::default();
    let mut active: Option<Active> = None;
    // Set after a `+CMT:`/`+CDS:` header: the next line is its PDU.
    let mut pending_pdu: Option<bool> = None;
    let mut chunk = [0u8; 1024];
    let far = || Instant::now() + Duration::from_secs(86400);

    loop {
        let deadline = active.as_ref().map(|a| a.deadline).unwrap_or_else(far);
        tokio::select! {
            read = reader.read(&mut chunk) => {
                let n = match read {
                    Ok(0) => {
                        tracing::warn!(modem = %modem_id, "serial stream closed");
                        break;
                    }
                    Ok(n) => n,
                    Err(e) => {
                        tracing::warn!(modem = %modem_id, error = %e, "serial read failed");
                        break;
                    }
                };
                buf.push(&chunk[..n]);
                loop {
                    if let Some(a) = active.as_mut() {
                        if matches!(a.phase, Phase::Prompt) && buf.take_prompt() {
                            let payload = a.cmd.payload.clone().unwrap_or_default();
                            let mut data = payload.into_bytes();
                            data.push(0x1A);
                            // From here on the modem may transmit: a timeout is ambiguous.
                            a.phase = Phase::Submitted;
                            a.deadline = Instant::now() + a.cmd.timeout;
                            if writer.write_all(&data).await.is_err() || writer.flush().await.is_err() {
                                break;
                            }
                            continue;
                        }
                    }
                    let Some(line) = buf.next_line() else { break };
                    tracing::trace!(modem = %modem_id, line = %line, "rx");
                    if let Some(is_message) = pending_pdu.take() {
                        let urc = if is_message { Urc::Message { pdu: line } } else { Urc::StatusReport { pdu: line } };
                        forward(&urc_tx, urc, &modem_id);
                        continue;
                    }
                    if let Some(is_message) = expects_pdu_line(&line) {
                        pending_pdu = Some(is_message);
                        continue;
                    }
                    match active.as_mut() {
                        Some(a) => {
                            if line.eq_ignore_ascii_case(&a.cmd.text) {
                                continue; // command echo
                            }
                            if let Some(fin) = classify_final(&line) {
                                let a = active.take().expect("active command present");
                                let _ = a.reply.send(match fin {
                                    Final::Ok => Ok(a.lines),
                                    Final::Error(e) => Err(AtError::Rejected(e)),
                                });
                                continue;
                            }
                            match parse_urc(&line) {
                                // A dial or answer command ends with the call result.
                                Some(Urc::CallEnded { .. }) if is_call_command(&a.cmd.text) => {
                                    let a = active.take().expect("active command present");
                                    let _ = a.reply.send(Err(AtError::Rejected(line.clone())));
                                    forward(&urc_tx, parse_urc(&line).expect("parsed above"), &modem_id);
                                }
                                Some(urc) if !is_expected(&a.cmd.text, &line) => forward(&urc_tx, urc, &modem_id),
                                _ => {
                                    if a.lines.len() < 512 {
                                        a.lines.push(line);
                                    }
                                }
                            }
                        }
                        None => match parse_urc(&line) {
                            Some(urc) => forward(&urc_tx, urc, &modem_id),
                            None => tracing::debug!(modem = %modem_id, line = %line, "ignoring unexpected modem output"),
                        },
                    }
                }
            }
            req = rx.recv(), if active.is_none() => {
                let Some((cmd, reply)) = req else { break };
                if reply.is_closed() {
                    continue; // caller gave up while the command was queued
                }
                if cmd.sensitive {
                    tracing::debug!(modem = %modem_id, "tx <redacted>");
                } else {
                    tracing::trace!(modem = %modem_id, command = %cmd.text, "tx");
                }
                let mut data = cmd.text.clone().into_bytes();
                data.push(b'\r');
                if writer.write_all(&data).await.is_err() || writer.flush().await.is_err() {
                    let _ = reply.send(Err(AtError::Closed));
                    break;
                }
                let (phase, wait) = if cmd.payload.is_some() { (Phase::Prompt, PROMPT_TIMEOUT.min(cmd.timeout)) } else { (Phase::Response, cmd.timeout) };
                active = Some(Active { deadline: Instant::now() + wait, cmd, reply, lines: Vec::new(), phase });
            }
            _ = tokio::time::sleep_until(deadline), if active.is_some() => {
                let a = active.take().expect("active command present");
                let ambiguous = matches!(a.phase, Phase::Submitted);
                if matches!(a.phase, Phase::Prompt) {
                    // Abort data entry so the modem returns to command mode.
                    let _ = writer.write_all(&[0x1B]).await;
                    let _ = writer.flush().await;
                }
                tracing::warn!(modem = %modem_id, ambiguous, "AT command timed out");
                let _ = a.reply.send(Err(AtError::Timeout { ambiguous }));
            }
        }
    }
    if let Some(a) = active.take() {
        let ambiguous = matches!(a.phase, Phase::Submitted);
        let _ = a.reply.send(if ambiguous {
            Err(AtError::Timeout { ambiguous: true })
        } else {
            Err(AtError::Closed)
        });
    }
    // Dropping `rx` fails queued commands; dropping `urc_tx` signals the session.
}

fn forward(tx: &mpsc::Sender<Urc>, urc: Urc, modem_id: &str) {
    if tx.try_send(urc).is_err() {
        tracing::warn!(modem = %modem_id, "unsolicited result queue full or closed; notification dropped (the periodic poll recovers stored messages)");
    }
}

fn is_call_command(cmd: &str) -> bool {
    let c = cmd.to_ascii_uppercase();
    c == "ATA" || c.starts_with("ATD")
}

/// Whether `line` is the information response of the running command rather
/// than an unsolicited code (for example `+CUSD: (0-2)` after `AT+CUSD=?`).
fn is_expected(cmd: &str, line: &str) -> bool {
    let c = cmd.to_ascii_uppercase();
    let Some(prefix) = line.split(':').next() else {
        return false;
    };
    let query = c.ends_with('?');
    query
        && c.trim_end_matches('?')
            .trim_end_matches('=')
            .ends_with(&prefix.to_ascii_uppercase()[1.min(prefix.len())..])
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};

    fn pair() -> (AtHandle, mpsc::Receiver<Urc>, DuplexStream) {
        let (a, b) = duplex(4096);
        let (h, u) = spawn(Box::new(a), "t".into());
        (h, u, b)
    }

    async fn expect(far: &mut DuplexStream, want: &[u8]) {
        let mut got = vec![0u8; want.len()];
        tokio::time::timeout(Duration::from_secs(2), far.read_exact(&mut got))
            .await
            .expect("timely")
            .unwrap();
        assert_eq!(got, want);
    }

    #[tokio::test]
    async fn command_with_echo_fragments_and_interleaved_urc() {
        let (h, mut urc, mut far) = pair();
        let task = tokio::spawn(async move { h.at("AT+CSQ", Duration::from_secs(2)).await });
        expect(&mut far, b"AT+CSQ\r").await;
        for part in [
            "AT+CSQ\r\r\n",
            "+CMTI: \"SM\",7\r\n+CS",
            "Q: 18,0\r\n",
            "\r\nOK\r\n",
        ] {
            far.write_all(part.as_bytes()).await.unwrap();
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(task.await.unwrap().unwrap(), vec!["+CSQ: 18,0".to_string()]);
        assert_eq!(
            urc.recv().await,
            Some(Urc::MessageStored {
                storage: "SM".into(),
                index: 7
            })
        );
    }

    #[tokio::test]
    async fn query_response_is_not_mistaken_for_urc() {
        let (h, mut urc, mut far) = pair();
        let task = tokio::spawn(async move { h.at("AT+CUSD=?", Duration::from_secs(2)).await });
        expect(&mut far, b"AT+CUSD=?\r").await;
        far.write_all(b"\r\n+CUSD: (0-2)\r\n\r\nOK\r\n")
            .await
            .unwrap();
        assert_eq!(
            task.await.unwrap().unwrap(),
            vec!["+CUSD: (0-2)".to_string()]
        );
        assert!(urc.try_recv().is_err());
    }

    #[tokio::test]
    async fn errors_and_timeouts() {
        let (h, _urc, mut far) = pair();
        let h2 = h.clone();
        let task = tokio::spawn(async move { h2.at("AT+BAD", Duration::from_secs(2)).await });
        expect(&mut far, b"AT+BAD\r").await;
        far.write_all(b"\r\n+CME ERROR: 100\r\n").await.unwrap();
        assert_eq!(
            task.await.unwrap(),
            Err(AtError::Rejected("+CME ERROR: 100".into()))
        );
        let r = h.at("AT", Duration::from_millis(60)).await;
        assert_eq!(r, Err(AtError::Timeout { ambiguous: false }));
    }

    #[tokio::test]
    async fn pdu_submission_and_ambiguity() {
        let (h, _urc, mut far) = pair();
        let cmd = |t: u64| AtCommand {
            payload: Some("0011AA".into()),
            ..AtCommand::new("AT+CMGS=3", Duration::from_millis(t))
        };
        let h2 = h.clone();
        let task = tokio::spawn(async move { h2.run(cmd(2000)).await });
        expect(&mut far, b"AT+CMGS=3\r").await;
        far.write_all(b"\r\n> ").await.unwrap();
        expect(&mut far, b"0011AA\x1a").await;
        far.write_all(b"\r\n+CMGS: 9\r\n\r\nOK\r\n").await.unwrap();
        assert_eq!(task.await.unwrap().unwrap(), vec!["+CMGS: 9".to_string()]);

        // Prompt received, payload sent, then silence: ambiguous.
        let h2 = h.clone();
        let task = tokio::spawn(async move { h2.run(cmd(80)).await });
        expect(&mut far, b"AT+CMGS=3\r").await;
        far.write_all(b"\r\n> ").await.unwrap();
        expect(&mut far, b"0011AA\x1a").await;
        assert_eq!(
            task.await.unwrap(),
            Err(AtError::Timeout { ambiguous: true })
        );

        // No prompt at all: aborted with ESC, definitely not sent.
        let h2 = h.clone();
        let task = tokio::spawn(async move { h2.run(cmd(80)).await });
        expect(&mut far, b"AT+CMGS=3\r").await;
        assert_eq!(
            task.await.unwrap(),
            Err(AtError::Timeout { ambiguous: false })
        );
        expect(&mut far, b"\x1b").await;
    }

    #[tokio::test]
    async fn two_line_urcs_and_close() {
        let (h, mut urc, mut far) = pair();
        far.write_all(b"\r\n+CMT: ,24\r\n0791AABBCC\r\n\r\n+CDS: 25\r\n0006AA\r\nRING\r\n")
            .await
            .unwrap();
        assert_eq!(
            urc.recv().await,
            Some(Urc::Message {
                pdu: "0791AABBCC".into()
            })
        );
        assert_eq!(
            urc.recv().await,
            Some(Urc::StatusReport {
                pdu: "0006AA".into()
            })
        );
        assert_eq!(urc.recv().await, Some(Urc::Ring));
        drop(far);
        assert_eq!(urc.recv().await, None);
        assert_eq!(
            h.at("AT", Duration::from_secs(1)).await,
            Err(AtError::Closed)
        );
    }

    #[tokio::test]
    async fn concurrent_callers_are_serialised() {
        let (h, _urc, mut far) = pair();
        let mut tasks = Vec::new();
        for i in 0..5 {
            let h = h.clone();
            tasks.push(tokio::spawn(async move {
                h.at(&format!("AT+T{i}"), Duration::from_secs(5)).await
            }));
        }
        // Answer whatever arrives, one command at a time; a second command
        // must never be written before the first is answered.
        for _ in 0..5 {
            let mut line = Vec::new();
            loop {
                let mut b = [0u8; 1];
                far.read_exact(&mut b).await.unwrap();
                if b[0] == b'\r' {
                    break;
                }
                line.push(b[0]);
            }
            let cmd = String::from_utf8(line).unwrap();
            assert!(
                cmd.starts_with("AT+T") && cmd.len() == 5,
                "interleaved write: {cmd:?}"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
            far.write_all(format!("\r\n{cmd}-R\r\n\r\nOK\r\n").as_bytes())
                .await
                .unwrap();
        }
        for (i, t) in tasks.into_iter().enumerate() {
            assert_eq!(t.await.unwrap().unwrap(), vec![format!("AT+T{i}-R")]);
        }
    }
}
