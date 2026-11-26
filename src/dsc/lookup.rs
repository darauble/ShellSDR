//! Online position lookup (group `lookup`, LKP-01..06): one HTTP GET per
//! identity to the configured endpoint, aprs.fi answer format
//! (spec/06-behaviour.md §5). The only module that touches the network
//! (NFR-04).

use std::time::Duration;

/// A position found online.
#[derive(Debug, Clone, PartialEq)]
pub struct Fix {
    pub lat: f64,
    pub lon: f64,
    /// UTC epoch the position was observed.
    pub time: Option<i64>,
    pub name: Option<String>,
}

/// Why a lookup gave no position.
#[derive(Debug, Clone, PartialEq)]
pub enum Failure {
    NotFound,
    /// `"result":"fail"`: bad key or target, with the service's reason.
    Refused(String),
    /// No answer: no network, refused connection, timeout.
    Network(String),
    /// An answer that is not the expected format.
    BadAnswer(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            Failure::NotFound => write!(f, "not found"),
            Failure::Refused(why) => write!(f, "refused by the service ({why})"),
            Failure::Network(why) => write!(f, "no answer ({why})"),
            Failure::BadAnswer(why) => write!(f, "unreadable answer ({why})"),
        }
    }
}

/// The request URL: `{mmsi}` and `{apikey}` replaced (LKP-06).
pub fn url(template: &str, mmsi: &str, key: &str) -> String {
    template
        .replace("{mmsi}", &encode(mmsi))
        .replace("{apikey}", &encode(key))
}

/// Percent-encoding of a URL query value (RFC 3986 unreserved kept).
fn encode(v: &str) -> String {
    v.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Reads an aprs.fi `loc` answer.
pub fn parse_aprs(body: &str) -> Result<Fix, Failure> {
    use serde_json::Value;
    let bad = |why: &str| Failure::BadAnswer(why.to_string());
    let v: Value = serde_json::from_str(body).map_err(|_| bad("not JSON"))?;
    if v["result"] == "fail" {
        return Err(Failure::Refused(
            v["description"]
                .as_str()
                .unwrap_or("no reason given")
                .to_string(),
        ));
    }
    // Numbers may come as JSON numbers or as text.
    let num = |x: &Value| {
        x.as_f64()
            .or_else(|| x.as_str().and_then(|t| t.trim().parse().ok()))
    };
    if num(&v["found"]) == Some(0.0) {
        return Err(Failure::NotFound);
    }
    let e = v["entries"].get(0).ok_or_else(|| bad("no entries"))?;
    let pick = |a: &str, b: &str| num(&e[a]).or_else(|| num(&e[b]));
    let lat = pick("lat", "latitude")
        .filter(|l| (-90.0..=90.0).contains(l))
        .ok_or_else(|| bad("no latitude"))?;
    let lon = pick("lng", "longitude")
        .filter(|l| (-180.0..=180.0).contains(l))
        .ok_or_else(|| bad("no longitude"))?;
    let time = pick("lasttime", "time").map(|t| t as i64);
    let name = e["name"]
        .as_str()
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .map(String::from);
    Ok(Fix {
        lat,
        lon,
        time,
        name,
    })
}

/// One lookup: GET, then parse. Blocks up to `timeout`; call it on a
/// worker thread (LKP-03).
pub fn fetch(template: &str, key: &str, mmsi: &str, timeout: Duration) -> Result<Fix, Failure> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        .http_status_as_error(false)
        .user_agent("dsqrust")
        .build()
        .into();
    let mut resp = agent
        .get(&url(template, mmsi, key))
        .call()
        .map_err(|e| Failure::Network(e.to_string()))?;
    let body = resp
        .body_mut()
        .with_config()
        .limit(1 << 20)
        .read_to_string()
        .map_err(|e| Failure::Network(e.to_string()))?;
    if !resp.status().is_success() && serde_json::from_str::<serde_json::Value>(&body).is_err() {
        return Err(Failure::BadAnswer(format!(
            "HTTP {}",
            resp.status().as_u16()
        )));
    }
    parse_aprs(&body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    const FOUND: &str = r#"{"command":"get","result":"ok","what":"loc","found":1,"entries":[{"name":"TEST VESSEL","mmsi":"477231700","lat":"52.58333","lng":"1.71667","time":"1792498000","lasttime":"1792498900"}]}"#;

    #[test]
    fn url_fills_in_the_identity_and_the_key() {
        assert_eq!(
            url(
                "https://h/api?name={mmsi}&apikey={apikey}",
                "477231700",
                "a b&c"
            ),
            "https://h/api?name=477231700&apikey=a%20b%26c"
        );
    }

    #[test]
    fn a_found_answer_gives_position_time_and_name() {
        assert_eq!(
            parse_aprs(FOUND),
            Ok(Fix {
                lat: 52.58333,
                lon: 1.71667,
                time: Some(1792498900),
                name: Some("TEST VESSEL".into())
            })
        );
        let alt =
            r#"{"result":"ok","found":1,"entries":[{"latitude":1.5,"longitude":-2.5,"time":7}]}"#;
        assert_eq!(
            parse_aprs(alt),
            Ok(Fix {
                lat: 1.5,
                lon: -2.5,
                time: Some(7),
                name: None
            })
        );
    }

    #[test]
    fn not_found_bad_key_and_garbage_are_told_apart() {
        assert_eq!(
            parse_aprs(r#"{"command":"get","result":"ok","what":"loc","found":0,"entries":[]}"#),
            Err(Failure::NotFound)
        );
        assert_eq!(
            parse_aprs(
                r#"{"command":"get","result":"fail","description":"authentication failed"}"#
            ),
            Err(Failure::Refused("authentication failed".into()))
        );
        assert!(matches!(parse_aprs("<html>"), Err(Failure::BadAnswer(_))));
        assert!(matches!(
            parse_aprs(r#"{"result":"ok","found":1,"entries":[{"lat":"x"}]}"#),
            Err(Failure::BadAnswer(_))
        ));
    }

    /// Serves one canned HTTP answer on 127.0.0.1; returns the URL template.
    fn stub(body: &'static str) -> String {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = c.read(&mut buf);
            let _ = write!(
                c,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        });
        format!("http://127.0.0.1:{port}/api/get?name={{mmsi}}&apikey={{apikey}}")
    }

    #[test]
    fn fetch_asks_the_endpoint_and_parses_the_answer() {
        let fix = fetch(&stub(FOUND), "k", "477231700", Duration::from_secs(5)).unwrap();
        assert_eq!(fix.name.as_deref(), Some("TEST VESSEL"));
    }

    #[test]
    fn nobody_listening_is_a_network_failure() {
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = l.local_addr().unwrap().port();
        drop(l);
        let r = fetch(
            &format!("http://127.0.0.1:{port}/?n={{mmsi}}"),
            "k",
            "1",
            Duration::from_secs(5),
        );
        assert!(matches!(r, Err(Failure::Network(_))), "{r:?}");
    }
}
