//! AT response framing and parsing: line assembly from partial serial reads,
//! final result classification, unsolicited result codes and field parsers.

/// Assembles lines from arbitrarily fragmented serial reads.
#[derive(Default)]
pub struct LineBuffer {
    buf: Vec<u8>,
}

const MAX_BUFFER: usize = 64 * 1024;

impl LineBuffer {
    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
        if self.buf.len() > MAX_BUFFER {
            // A modem never legitimately sends this much without a line end.
            let cut = self.buf.len() - MAX_BUFFER / 2;
            self.buf.drain(..cut);
        }
    }

    /// Next complete, non-empty line without its terminator.
    pub fn next_line(&mut self) -> Option<String> {
        loop {
            let pos = self.buf.iter().position(|b| *b == b'\n')?;
            let raw: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&raw)
                .trim_matches(['\r', '\n', '\0'])
                .trim_end()
                .to_string();
            if !line.is_empty() {
                return Some(line);
            }
        }
    }

    /// Consume a `> ` data prompt if one is waiting (it has no line ending).
    pub fn take_prompt(&mut self) -> bool {
        let start = self
            .buf
            .iter()
            .position(|b| !matches!(b, b'\r' | b'\n' | b' '))
            .unwrap_or(self.buf.len());
        if self.buf.get(start) == Some(&b'>') && !self.buf[start..].contains(&b'\n') {
            self.buf.clear();
            true
        } else {
            false
        }
    }
}

/// Final result code of a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Final {
    Ok,
    /// `ERROR`, `+CME ERROR: x` or `+CMS ERROR: x`
    Error(String),
}

pub fn classify_final(line: &str) -> Option<Final> {
    if line == "OK" {
        Some(Final::Ok)
    } else if line == "ERROR"
        || line.starts_with("+CME ERROR:")
        || line.starts_with("+CMS ERROR:")
        || line == "COMMAND NOT SUPPORT"
    {
        Some(Final::Error(line.to_string()))
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Domain {
    Circuit,
    Gprs,
    Eps,
}

/// Unsolicited result codes the gateway reacts to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Urc {
    /// `+CMTI`: a message was stored at `index`.
    MessageStored {
        storage: String,
        index: u32,
    },
    /// `+CDSI`: a status report was stored.
    ReportStored {
        storage: String,
        index: u32,
    },
    /// `+CMT` with its PDU line.
    Message {
        pdu: String,
    },
    /// `+CDS` with its PDU line.
    StatusReport {
        pdu: String,
    },
    Registration {
        domain: Domain,
        stat: u8,
    },
    Ring,
    CallerId {
        number: String,
    },
    /// `NO CARRIER`, `BUSY` or `NO ANSWER`
    CallEnded {
        reason: String,
    },
    Ussd {
        status: u8,
        text: Option<String>,
        dcs: Option<u8>,
    },
    /// `+CPIN: ...` reported spontaneously (SIM inserted or removed).
    SimState(String),
    /// The modem (re)booted: `RDY`, `+CFUN: 1` and similar.
    Booted,
    /// `NORMAL POWER DOWN` and under/over-voltage shutdowns.
    PowerDown(String),
    Other(String),
}

/// Split comma separated fields, honouring double quotes.
pub fn split_fields(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    for c in s.chars() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                out.push(cur.trim().to_string());
                cur.clear();
            }
            _ => cur.push(c),
        }
    }
    out.push(cur.trim().to_string());
    out
}

fn after<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
    line.strip_prefix(prefix).map(str::trim)
}

/// Header lines whose following line carries a PDU.
pub fn expects_pdu_line(line: &str) -> Option<bool> {
    if line.starts_with("+CMT:") {
        Some(true)
    } else if line.starts_with("+CDS:") {
        Some(false)
    } else {
        None
    }
}

/// Parse a line as an unsolicited result code. Returns `None` for lines that
/// are not recognised as unsolicited (they belong to a command response).
pub fn parse_urc(line: &str) -> Option<Urc> {
    for (prefix, report) in [("+CMTI:", false), ("+CDSI:", true)] {
        if let Some(rest) = after(line, prefix) {
            let f = split_fields(rest);
            let index = f.get(1).and_then(|i| i.parse().ok())?;
            let storage = f[0].clone();
            return Some(if report {
                Urc::ReportStored { storage, index }
            } else {
                Urc::MessageStored { storage, index }
            });
        }
    }
    for (prefix, domain) in [
        ("+CREG:", Domain::Circuit),
        ("+CGREG:", Domain::Gprs),
        ("+CEREG:", Domain::Eps),
    ] {
        if let Some(rest) = after(line, prefix) {
            let f = split_fields(rest);
            // Unsolicited form is `<stat>[,<lac>,<ci>...]`; the query response
            // is `<n>,<stat>[,...]`. With location info disabled (n=1) a
            // single field identifies the unsolicited form.
            if f.len() == 1
                || f.get(1)
                    .map(|s| {
                        s.chars()
                            .any(|c| c.is_ascii_hexdigit() && !c.is_ascii_digit())
                            || s.len() > 2
                    })
                    .unwrap_or(false)
            {
                return Some(Urc::Registration {
                    domain,
                    stat: f[0].parse().ok()?,
                });
            }
            return None;
        }
    }
    if line == "RING" || line.starts_with("+CRING:") {
        return Some(Urc::Ring);
    }
    if let Some(rest) = after(line, "+CLIP:") {
        return Some(Urc::CallerId {
            number: split_fields(rest).into_iter().next().unwrap_or_default(),
        });
    }
    if matches!(line, "NO CARRIER" | "BUSY" | "NO ANSWER" | "NO DIALTONE") {
        return Some(Urc::CallEnded {
            reason: line.to_lowercase().replace(' ', "_"),
        });
    }
    if let Some(rest) = after(line, "+CUSD:") {
        let f = split_fields(rest);
        return Some(Urc::Ussd {
            status: f.first().and_then(|s| s.parse().ok()).unwrap_or(0),
            text: f.get(1).cloned(),
            dcs: f.get(2).and_then(|s| s.parse().ok()),
        });
    }
    if matches!(
        line,
        "RDY" | "+CFUN: 1" | "Call Ready" | "SMS Ready" | "+CPIN: READY" | "POWERED DOWN"
    ) || line.starts_with("*PSUTTZ")
    {
        if line == "POWERED DOWN" {
            return Some(Urc::PowerDown(line.to_string()));
        }
        if line == "+CPIN: READY" {
            return Some(Urc::SimState("READY".into()));
        }
        return Some(if line == "RDY" {
            Urc::Booted
        } else {
            Urc::Other(line.to_string())
        });
    }
    if line.contains("POWER DOWN") {
        return Some(Urc::PowerDown(line.to_string()));
    }
    if let Some(rest) = after(line, "+CPIN:") {
        return Some(Urc::SimState(rest.to_string()));
    }
    for p in [
        "+CTZV:",
        "+CIEV:",
        "*PSNWID:",
        "+CCWA:",
        "+CGEV:",
        "+QIND:",
        "+CME ERROR: SIM",
        "DST:",
        "+CSQN:",
        "+BTPAIRING:",
        "+UGNSINF:",
    ] {
        if line.starts_with(p) {
            return Some(Urc::Other(line.to_string()));
        }
    }
    None
}

/// `+CSQ: <rssi>,<ber>` to (raw, dBm, percent). 99 means unknown.
pub fn parse_csq(lines: &[String]) -> Option<(u8, Option<i32>, Option<u8>)> {
    let f = split_fields(lines.iter().find_map(|l| after(l, "+CSQ:"))?);
    let raw: u8 = f.first()?.parse().ok()?;
    if raw > 31 {
        return Some((raw, None, None));
    }
    Some((
        raw,
        Some(-113 + 2 * raw as i32),
        Some((raw as u32 * 100 / 31) as u8),
    ))
}

/// Registration status from a `+CREG?`-style query response.
pub fn parse_reg(lines: &[String], prefix: &str) -> Option<u8> {
    let f = split_fields(lines.iter().find_map(|l| after(l, prefix))?);
    f.get(1).or(f.first())?.parse().ok()
}

pub fn registration_name(stat: u8) -> &'static str {
    match stat {
        0 => "not_registered",
        1 => "home",
        2 => "searching",
        3 => "denied",
        5 => "roaming",
        6 | 7 => "sms_only",
        8 => "emergency_only",
        _ => "unknown",
    }
}

pub fn is_registered(stat: u8) -> bool {
    matches!(stat, 1 | 5 | 6 | 7)
}

/// `+COPS: <mode>,<format>,"<oper>"[,<act>]` to (operator, access technology).
pub fn parse_cops(lines: &[String]) -> Option<(String, Option<&'static str>)> {
    let f = split_fields(lines.iter().find_map(|l| after(l, "+COPS:"))?);
    let oper = f.get(2)?.clone();
    let act = f
        .get(3)
        .and_then(|a| a.parse::<u8>().ok())
        .map(|a| match a {
            0 | 1 | 3 => "GSM",
            2 | 4 | 5 | 6 => "UMTS",
            7 => "LTE",
            8 => "LTE-M",
            9 => "NB-IoT",
            _ => "unknown",
        });
    Some((oper, act))
}

/// `+CPMS: "SM",used,total,...` to (storage, used, total).
pub fn parse_cpms(lines: &[String]) -> Option<(String, u32, u32)> {
    let f = split_fields(lines.iter().find_map(|l| after(l, "+CPMS:"))?);
    if f.first()?.parse::<u32>().is_ok() {
        // Response to the set command omits the storage names.
        return Some((String::new(), f[0].parse().ok()?, f.get(1)?.parse().ok()?));
    }
    Some((
        f[0].clone(),
        f.get(1)?.parse().ok()?,
        f.get(2)?.parse().ok()?,
    ))
}

fn looks_like_pdu(s: &str) -> bool {
    s.len() >= 4 && s.len() % 2 == 0 && s.chars().all(|c| c.is_ascii_hexdigit())
}

/// `+CMGL` listing in PDU mode to (index, PDU) pairs. Only received
/// messages (status 0 or 1) are returned; drafts and sent copies stored on
/// the SIM are left untouched.
pub fn parse_cmgl(lines: &[String]) -> Vec<(u32, String)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        if let Some(rest) = after(&lines[i], "+CMGL:") {
            let f = split_fields(rest);
            let received = matches!(f.get(1).map(String::as_str), Some("0" | "1"));
            if let (Some(idx), Some(pdu)) = (
                f.first().and_then(|s| s.parse().ok()),
                lines.get(i + 1).filter(|l| looks_like_pdu(l)),
            ) {
                if received {
                    out.push((idx, pdu.clone()));
                }
                i += 1;
            }
        }
        i += 1;
    }
    out
}

/// PDU from a `+CMGR` response.
pub fn parse_cmgr(lines: &[String]) -> Option<String> {
    let pos = lines.iter().position(|l| l.starts_with("+CMGR:"))?;
    lines.get(pos + 1).filter(|l| looks_like_pdu(l)).cloned()
}

/// Message reference from `+CMGS: <mr>`.
pub fn parse_cmgs(lines: &[String]) -> Option<u8> {
    split_fields(lines.iter().find_map(|l| after(l, "+CMGS:"))?)
        .first()?
        .parse()
        .ok()
}

/// First response line that is not an echo of the command, with an optional
/// `+PREFIX:` removed (used for identification commands).
pub fn info_line(lines: &[String], command: &str) -> Option<String> {
    lines
        .iter()
        .find(|l| !l.eq_ignore_ascii_case(command))
        .map(|l| match l.split_once(':') {
            Some((p, rest)) if p.starts_with('+') => rest.trim().trim_matches('"').to_string(),
            _ => l.trim().to_string(),
        })
}

/// Values of a `(a,b,c)` or `(a-b)` test-command list.
pub fn parse_list(s: &str) -> Vec<String> {
    let inner = s.trim().trim_start_matches('(').trim_end_matches(')');
    let mut out = Vec::new();
    for item in split_fields(inner) {
        match item.split_once('-') {
            Some((a, b)) => match (a.parse::<u32>(), b.parse::<u32>()) {
                (Ok(a), Ok(b)) if a <= b && b - a < 64 => {
                    out.extend((a..=b).map(|v| v.to_string()))
                }
                _ => out.push(item),
            },
            None if !item.is_empty() => out.push(item),
            None => {}
        }
    }
    out
}

/// Parenthesised groups of a test-command response, e.g. `(0,1),(0-3)`.
pub fn parse_groups(s: &str) -> Vec<Vec<String>> {
    let mut groups = Vec::new();
    let mut depth = 0;
    let mut cur = String::new();
    for c in s.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.clear();
            }
            ')' => {
                if depth > 0 {
                    groups.push(parse_list(&cur));
                }
                depth = 0;
            }
            _ if depth > 0 => cur.push(c),
            _ => {}
        }
    }
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(l: &[&str]) -> Vec<String> {
        l.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn assembles_lines_from_fragments() {
        let mut b = LineBuffer::default();
        for chunk in ["\r\n+CS", "Q: 18", ",0\r", "\n\r\nO", "K\r\n"] {
            b.push(chunk.as_bytes());
        }
        assert_eq!(b.next_line().as_deref(), Some("+CSQ: 18,0"));
        assert_eq!(b.next_line().as_deref(), Some("OK"));
        assert_eq!(b.next_line(), None);
    }

    #[test]
    fn detects_prompt_without_newline() {
        let mut b = LineBuffer::default();
        b.push(b"\r\n");
        assert!(!b.take_prompt());
        b.push(b"> ");
        assert!(b.take_prompt());
        b.push(b"> not a prompt\r\n");
        assert!(!b.take_prompt());
    }

    #[test]
    fn bounded_buffer() {
        let mut b = LineBuffer::default();
        for _ in 0..100 {
            b.push(&[b'x'; 4096]);
        }
        assert!(b.buf.len() <= MAX_BUFFER);
    }

    #[test]
    fn finals() {
        assert_eq!(classify_final("OK"), Some(Final::Ok));
        assert!(matches!(
            classify_final("+CMS ERROR: 500"),
            Some(Final::Error(_))
        ));
        assert!(matches!(classify_final("ERROR"), Some(Final::Error(_))));
        assert_eq!(classify_final("+CSQ: 1,1"), None);
    }

    #[test]
    fn urcs() {
        assert_eq!(
            parse_urc("+CMTI: \"SM\",3"),
            Some(Urc::MessageStored {
                storage: "SM".into(),
                index: 3
            })
        );
        assert_eq!(
            parse_urc("+CDSI: \"SR\",1"),
            Some(Urc::ReportStored {
                storage: "SR".into(),
                index: 1
            })
        );
        assert_eq!(
            parse_urc("+CREG: 5"),
            Some(Urc::Registration {
                domain: Domain::Circuit,
                stat: 5
            })
        );
        assert_eq!(
            parse_urc("+CREG: 1,\"00A1\",\"1F2B\""),
            Some(Urc::Registration {
                domain: Domain::Circuit,
                stat: 1
            })
        );
        assert_eq!(
            parse_urc("+CREG: 0,5"),
            None,
            "query response is not unsolicited"
        );
        assert_eq!(
            parse_urc("+CGREG: 2"),
            Some(Urc::Registration {
                domain: Domain::Gprs,
                stat: 2
            })
        );
        assert_eq!(parse_urc("RING"), Some(Urc::Ring));
        assert_eq!(
            parse_urc("+CLIP: \"+4500000000\",145,\"\",0,\"\",0"),
            Some(Urc::CallerId {
                number: "+4500000000".into()
            })
        );
        assert_eq!(
            parse_urc("NO CARRIER"),
            Some(Urc::CallEnded {
                reason: "no_carrier".into()
            })
        );
        assert_eq!(
            parse_urc("+CUSD: 0,\"Balance: 10\",15"),
            Some(Urc::Ussd {
                status: 0,
                text: Some("Balance: 10".into()),
                dcs: Some(15)
            })
        );
        assert_eq!(
            parse_urc("NORMAL POWER DOWN"),
            Some(Urc::PowerDown("NORMAL POWER DOWN".into()))
        );
        assert_eq!(parse_urc("RDY"), Some(Urc::Booted));
        assert_eq!(
            parse_urc("+CPIN: NOT READY"),
            Some(Urc::SimState("NOT READY".into()))
        );
        assert_eq!(parse_urc("SIMCOM_SIM868"), None);
        assert_eq!(expects_pdu_line("+CMT: ,24"), Some(true));
        assert_eq!(expects_pdu_line("+CDS: 25"), Some(false));
    }

    #[test]
    fn field_parsers() {
        assert_eq!(
            parse_csq(&v(&["+CSQ: 18,0"])),
            Some((18, Some(-77), Some(58)))
        );
        assert_eq!(parse_csq(&v(&["+CSQ: 99,99"])), Some((99, None, None)));
        assert_eq!(parse_reg(&v(&["+CREG: 0,5"]), "+CREG:"), Some(5));
        assert_eq!(
            parse_reg(&v(&["+CREG: 2,1,\"00A1\",\"1F2B\""]), "+CREG:"),
            Some(1)
        );
        assert_eq!(
            parse_cops(&v(&["+COPS: 0,0,\"Example Mobil\""])),
            Some(("Example Mobil".into(), None))
        );
        assert_eq!(
            parse_cops(&v(&["+COPS: 0,0,\"Op\",7"])),
            Some(("Op".into(), Some("LTE")))
        );
        assert_eq!(parse_cops(&v(&["+COPS: 0"])), None);
        assert_eq!(
            parse_cpms(&v(&["+CPMS: \"SM_P\",2,50,\"SM_P\",2,50,\"SM_P\",2,50"])),
            Some(("SM_P".into(), 2, 50))
        );
        assert_eq!(
            parse_cpms(&v(&["+CPMS: 3,30,3,30,3,30"])),
            Some((String::new(), 3, 30))
        );
        assert_eq!(parse_cmgs(&v(&["+CMGS: 42"])), Some(42));
        assert_eq!(
            parse_cmgr(&v(&["+CMGR: 0,,23", "0791AABB"])),
            Some("0791AABB".into())
        );
        assert_eq!(parse_cmgr(&v(&["+CMGR: 0,,23"])), None);
        let list = parse_cmgl(&v(&[
            "+CMGL: 1,0,,23",
            "07911234",
            "+CMGL: 4,1,,30",
            "0791ABCD",
            "+CMGL: 5,1,,30",
            "+CMGL: 6,3,,30",
            "0791FFFF",
        ]));
        assert_eq!(
            list,
            vec![(1, "07911234".to_string()), (4, "0791ABCD".to_string())]
        );
        assert_eq!(
            info_line(&v(&["AT+CGMM", "SIMCOM_SIM868"]), "AT+CGMM").as_deref(),
            Some("SIMCOM_SIM868")
        );
        assert_eq!(
            info_line(&v(&["+CCID: \"8945000000\""]), "AT+CCID").as_deref(),
            Some("8945000000")
        );
        assert_eq!(
            parse_groups("(0-3),(0,1),(\"IRA\",\"GSM\")"),
            vec![v(&["0", "1", "2", "3"]), v(&["0", "1"]), v(&["IRA", "GSM"])]
        );
        assert_eq!(parse_groups("(),(0,1200,115200)")[1].len(), 3);
    }
}
