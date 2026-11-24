//! MMS support: decoding of WAP Push MMS notifications (M-Notification.ind)
//! and optional retrieval of the message from the configured MMSC.
//!
//! Retrieval only ever contacts the host named in `mms.mmsc_url`; URLs taken
//! from received messages are never followed anywhere else. Downloaded
//! content is stored as an opaque attachment and never interpreted.

use crate::config::Mms as MmsConfig;
use serde::Serialize;

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MmsNotification {
    pub transaction_id: Option<String>,
    pub version: Option<String>,
    pub from: Option<String>,
    pub subject: Option<String>,
    pub message_class: Option<String>,
    pub size_bytes: Option<u64>,
    pub expiry_seconds: Option<u64>,
    pub content_location: Option<String>,
}

struct Cur<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Cur<'a> {
    fn u8(&mut self) -> Option<u8> {
        let v = *self.b.get(self.p)?;
        self.p += 1;
        Some(v)
    }
    fn peek(&self) -> Option<u8> {
        self.b.get(self.p).copied()
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.b.get(self.p..self.p.checked_add(n)?)?;
        self.p += n;
        Some(s)
    }
    fn uintvar(&mut self) -> Option<usize> {
        let mut v = 0usize;
        for _ in 0..5 {
            let b = self.u8()?;
            v = (v << 7) | (b & 0x7F) as usize;
            if b & 0x80 == 0 {
                return Some(v);
            }
        }
        None
    }
    /// Value-length: short length octet or 0x1F followed by a uintvar.
    fn value_length(&mut self) -> Option<usize> {
        match self.u8()? {
            31 => self.uintvar(),
            n if n < 31 => Some(n as usize),
            _ => None,
        }
    }
    fn text(&mut self) -> Option<String> {
        let rest = self.b.get(self.p..)?;
        let end = rest.iter().position(|b| *b == 0)?;
        let mut s = &rest[..end];
        if s.first() == Some(&0x7F) {
            s = &s[1..];
        }
        self.p += end + 1;
        Some(
            String::from_utf8_lossy(s)
                .chars()
                .filter(|c| !c.is_control())
                .take(512)
                .collect(),
        )
    }
    fn long_int(&mut self) -> Option<u64> {
        let n = self.u8()? as usize;
        if n == 0 || n > 8 {
            return None;
        }
        Some(self.take(n)?.iter().fold(0u64, |a, b| (a << 8) | *b as u64))
    }
    /// Encoded-string-value: plain text or value-length + charset + text.
    fn encoded_string(&mut self) -> Option<String> {
        match self.peek()? {
            b if b <= 31 => {
                let len = self.value_length()?;
                let body = self.take(len)?;
                let mut c = Cur { b: body, p: 0 };
                // charset: short integer or long integer
                match c.peek()? {
                    b if b & 0x80 != 0 => {
                        c.u8();
                    }
                    _ => {
                        c.long_int()?;
                    }
                }
                let rest = &body[c.p..];
                let rest = rest.strip_suffix(&[0]).unwrap_or(rest);
                Some(
                    String::from_utf8_lossy(rest)
                        .chars()
                        .filter(|c| !c.is_control())
                        .take(512)
                        .collect(),
                )
            }
            _ => self.text(),
        }
    }
}

/// Decode a WAP Push user data payload. Returns the notification when the
/// push carries an MMS M-Notification.ind, `None` for any other push.
pub fn parse_wap_push(data: &[u8]) -> Option<MmsNotification> {
    let mut c = Cur { b: data, p: 0 };
    c.u8()?; // transaction id
    if c.u8()? != 0x06 {
        return None; // not a Push PDU
    }
    let headers_len = c.uintvar()?;
    let headers = c.take(headers_len)?;
    let body = &data[c.p..];
    let is_mms = headers.first() == Some(&0xBE)
        || headers
            .windows(31)
            .any(|w| w == b"application/vnd.wap.mms-message")
        || body.starts_with(&[0x8C, 0x82]);
    if !is_mms {
        return None;
    }
    parse_notification(body)
}

/// Parse MMS PDU headers of an M-Notification.ind.
pub fn parse_notification(body: &[u8]) -> Option<MmsNotification> {
    let mut c = Cur { b: body, p: 0 };
    let mut n = MmsNotification::default();
    let mut is_notification = false;
    while let Some(field) = c.u8() {
        if field & 0x80 == 0 {
            break; // application-specific text header: stop, lengths unknown
        }
        let parsed: Option<()> = (|| {
            match field & 0x7F {
                0x0C => is_notification = c.u8()? == 0x82,
                0x18 => n.transaction_id = Some(c.text()?),
                0x0D => {
                    let v = c.u8()? & 0x7F;
                    n.version = Some(format!("{}.{}", v >> 4, v & 0x0F));
                }
                0x09 => {
                    let len = c.value_length()?;
                    let inner = c.take(len)?;
                    if inner.first() == Some(&0x80) {
                        let mut ic = Cur {
                            b: &inner[1..],
                            p: 0,
                        };
                        let addr = ic.encoded_string().or_else(|| {
                            Some(
                                String::from_utf8_lossy(&inner[1..])
                                    .trim_end_matches('\0')
                                    .to_string(),
                            )
                        })?;
                        n.from = Some(addr.split('/').next().unwrap_or("").to_string());
                    }
                }
                0x16 => n.subject = Some(c.encoded_string()?),
                0x0A => {
                    n.message_class = Some(match c.peek()? {
                        0x80 => "personal".into(),
                        0x81 => "advertisement".into(),
                        0x82 => "informational".into(),
                        0x83 => "auto".into(),
                        b if b & 0x80 != 0 => "unknown".into(),
                        _ => return c.text().map(|t| n.message_class = Some(t)),
                    });
                    c.u8();
                }
                0x0E => n.size_bytes = Some(c.long_int()?),
                0x08 => {
                    let len = c.value_length()?;
                    let inner = c.take(len)?;
                    let mut ic = Cur { b: inner, p: 0 };
                    let token = ic.u8()?;
                    let v = ic.long_int()?;
                    // 0x81: relative seconds; 0x80: absolute time (kept as-is)
                    n.expiry_seconds = (token == 0x81).then_some(v);
                }
                0x03 => n.content_location = Some(c.text()?),
                // Single-octet headers: delivery report, priority, read reply,
                // report allowed, status, sender visibility, distribution.
                0x06 | 0x0F | 0x10 | 0x11 | 0x14 | 0x15 | 0x31 => {
                    c.u8()?;
                }
                _ => return None,
            }
            Some(())
        })();
        if parsed.is_none() {
            break;
        }
    }
    is_notification.then_some(n)
}

#[derive(Debug, thiserror::Error)]
pub enum FetchError {
    #[error("MMS retrieval is disabled")]
    Disabled,
    #[error("content location is not on the configured MMSC host")]
    ForeignHost,
    #[error("invalid content location")]
    InvalidUrl,
    #[error("attachment exceeds the configured size limit")]
    TooLarge,
    #[error("download failed: {0}")]
    Http(String),
}

/// Check that a content location may be fetched: http(s), no credentials, and
/// the same host as the configured MMSC.
pub fn validate_location(cfg: &MmsConfig, location: &str) -> Result<reqwest::Url, FetchError> {
    if !cfg.enabled || !cfg.download_attachments {
        return Err(FetchError::Disabled);
    }
    let url = reqwest::Url::parse(location).map_err(|_| FetchError::InvalidUrl)?;
    let mmsc = reqwest::Url::parse(&cfg.mmsc_url).map_err(|_| FetchError::Disabled)?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err(FetchError::InvalidUrl);
    }
    match (url.host_str(), mmsc.host_str()) {
        (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => Ok(url),
        _ => Err(FetchError::ForeignHost),
    }
}

/// Download the MMS PDU (M-Retrieve.conf) from the carrier MMSC.
pub async fn fetch(cfg: &MmsConfig, location: &str) -> Result<Vec<u8>, FetchError> {
    let url = validate_location(cfg, location)?;
    let limit = cfg.max_attachment_size_mb.max(1) * 1024 * 1024;
    let mut builder = reqwest::Client::builder()
        .user_agent(crate::integrations::USER_AGENT)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(60));
    if !cfg.proxy.is_empty() {
        builder = builder
            .proxy(reqwest::Proxy::all(&cfg.proxy).map_err(|e| FetchError::Http(e.to_string()))?);
    }
    let client = builder
        .build()
        .map_err(|e| FetchError::Http(e.to_string()))?;
    let mut resp = client
        .get(url)
        .header("Accept", "application/vnd.wap.mms-message, */*")
        .send()
        .await
        .map_err(|e| FetchError::Http(e.without_url().to_string()))?;
    if !resp.status().is_success() {
        return Err(FetchError::Http(format!(
            "HTTP status {}",
            resp.status().as_u16()
        )));
    }
    if resp.content_length().map(|l| l > limit).unwrap_or(false) {
        return Err(FetchError::TooLarge);
    }
    let mut out = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| FetchError::Http(e.without_url().to_string()))?
    {
        if (out.len() + chunk.len()) as u64 > limit {
            return Err(FetchError::TooLarge);
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<u8> {
        let mut v = vec![0x01, 0x06]; // TID, Push
        let headers: Vec<u8> = vec![0xBE, 0xAF, 0x84]; // content type MMS, X-Wap-Application-Id
        v.push(headers.len() as u8);
        v.extend(&headers);
        v.extend([0x8C, 0x82]); // m-notification-ind
        v.extend([0x98]);
        v.extend(b"TX123\0");
        v.extend([0x8D, 0x92]); // version 1.2
        let from = b"+4500000000/TYPE=PLMN\0";
        v.extend([0x89, (from.len() + 1) as u8, 0x80]);
        v.extend(from);
        v.extend([0x96]);
        v.extend(b"Holiday photo\0");
        v.extend([0x8A, 0x80]); // personal
        v.extend([0x8E, 0x02, 0x30, 0x00]); // size 12288
        v.extend([0x88, 0x05, 0x81, 0x03, 0x01, 0x51, 0x80]); // relative 86400
        v.extend([0x83]);
        v.extend(b"http://mmsc.example.invalid/mms/abc123\0");
        v
    }

    #[test]
    fn parses_mms_notification() {
        let n = parse_wap_push(&sample()).unwrap();
        assert_eq!(n.transaction_id.as_deref(), Some("TX123"));
        assert_eq!(n.version.as_deref(), Some("1.2"));
        assert_eq!(n.from.as_deref(), Some("+4500000000"));
        assert_eq!(n.subject.as_deref(), Some("Holiday photo"));
        assert_eq!(n.message_class.as_deref(), Some("personal"));
        assert_eq!(n.size_bytes, Some(12288));
        assert_eq!(n.expiry_seconds, Some(86400));
        assert_eq!(
            n.content_location.as_deref(),
            Some("http://mmsc.example.invalid/mms/abc123")
        );
    }

    #[test]
    fn rejects_other_pushes_and_never_panics() {
        assert!(parse_wap_push(&[0x01, 0x06, 0x01, 0xAE, 0x02, 0x05]).is_none());
        assert!(parse_wap_push(&[0x01, 0x04]).is_none());
        let s = sample();
        for len in 0..s.len() {
            let _ = parse_wap_push(&s[..len]);
        }
        let mut state = 7u64;
        for _ in 0..5000 {
            let buf: Vec<u8> = (0..(state % 90) as usize)
                .map(|_| {
                    state = state
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    (state >> 33) as u8
                })
                .collect();
            let _ = parse_wap_push(&buf);
            let _ = parse_notification(&buf);
        }
    }

    #[test]
    fn retrieval_is_restricted_to_the_mmsc_host() {
        let mut cfg = MmsConfig {
            enabled: true,
            download_attachments: true,
            mmsc_url: "http://mmsc.example.invalid/servlets/mms".into(),
            ..Default::default()
        };
        assert!(validate_location(&cfg, "http://mmsc.example.invalid/mms/abc").is_ok());
        assert!(matches!(
            validate_location(&cfg, "http://evil.example.invalid/mms/abc"),
            Err(FetchError::ForeignHost)
        ));
        assert!(matches!(
            validate_location(&cfg, "http://mmsc.example.invalid.evil.test/x"),
            Err(FetchError::ForeignHost)
        ));
        assert!(matches!(
            validate_location(&cfg, "file:///etc/passwd"),
            Err(FetchError::InvalidUrl)
        ));
        assert!(matches!(
            validate_location(&cfg, "http://user:pw@mmsc.example.invalid/x"),
            Err(FetchError::InvalidUrl)
        ));
        assert!(matches!(
            validate_location(&cfg, "http://127.0.0.1/admin"),
            Err(FetchError::ForeignHost)
        ));
        cfg.download_attachments = false;
        assert!(matches!(
            validate_location(&cfg, "http://mmsc.example.invalid/mms/abc"),
            Err(FetchError::Disabled)
        ));
    }
}
