use std::{collections::HashSet, net::Ipv6Addr};

use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde::{Deserialize, Serialize};

pub const MAX_IMPORT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_IMPORT_LINES: usize = 10_000;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Http,
    Https,
    Socks4,
    Socks5,
}

impl Protocol {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Http => "http",
            Self::Https => "https",
            Self::Socks4 => "socks4",
            Self::Socks5 => "socks5",
        }
    }

    pub fn parse(value: &str) -> Result<Self, ParseError> {
        match value.to_ascii_lowercase().as_str() {
            "http" => Ok(Self::Http),
            "https" => Ok(Self::Https),
            "socks4" => Ok(Self::Socks4),
            "socks5" => Ok(Self::Socks5),
            _ => Err(ParseError::new(
                "unsupported_protocol",
                "Use http, https, socks4 or socks5",
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    #[default]
    Auto,
    Url,
    HttpUrl,
    Socks5Url,
    CredentialsAt,
    Csv,
    CredentialsFirst,
    HostFirst,
    HostPort,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Proxy {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
}

impl Proxy {
    fn new(
        protocol: Protocol,
        host: &str,
        port: &str,
        username: &str,
        password: &str,
    ) -> Result<Self, ParseError> {
        let host = normalize_host(host)?;
        if port.is_empty() || !port.bytes().all(|c| c.is_ascii_digit()) {
            return Err(ParseError::new(
                "invalid_port",
                "Port must be an integer between 1 and 65535",
            ));
        }
        let port = port
            .parse::<u16>()
            .ok()
            .filter(|p| *p != 0)
            .ok_or(ParseError::new(
                "invalid_port",
                "Port must be an integer between 1 and 65535",
            ))?;
        if username.len() > 512
            || password.len() > 1024
            || username
                .chars()
                .chain(password.chars())
                .any(char::is_control)
        {
            return Err(ParseError::new(
                "invalid_credentials",
                "Credentials are too long or contain control characters",
            ));
        }
        if username.is_empty() && !password.is_empty() {
            return Err(ParseError::new(
                "invalid_credentials",
                "A password requires a username",
            ));
        }
        Ok(Self {
            protocol,
            host,
            port,
            username: username.into(),
            password: password.into(),
        })
    }

    pub fn endpoint(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    pub fn export(&self, format: Format) -> Result<String, ParseError> {
        let endpoint = self.endpoint();
        let incompatible = || {
            ParseError::new(
                "incompatible_format",
                "This format cannot preserve these credentials or protocol; use URL or CSV",
            )
        };
        if format == Format::HttpUrl && self.protocol != Protocol::Http
            || format == Format::Socks5Url && self.protocol != Protocol::Socks5
        {
            return Err(incompatible());
        }
        match format {
            Format::Auto | Format::Url | Format::HttpUrl | Format::Socks5Url => {
                let auth = if self.username.is_empty() {
                    String::new()
                } else {
                    format!(
                        "{}:{}@",
                        utf8_percent_encode(&self.username, NON_ALPHANUMERIC),
                        utf8_percent_encode(&self.password, NON_ALPHANUMERIC)
                    )
                };
                Ok(format!("{}://{}{}", self.protocol.as_str(), auth, endpoint))
            }
            Format::Csv => {
                let mut writer = csv::WriterBuilder::new()
                    .has_headers(false)
                    .quote_style(csv::QuoteStyle::Always)
                    .from_writer(Vec::new());
                writer
                    .write_record([
                        &self.host,
                        &self.port.to_string(),
                        &self.username,
                        &self.password,
                    ])
                    .map_err(|_| incompatible())?;
                let bytes = writer.into_inner().map_err(|_| incompatible())?;
                Ok(String::from_utf8(bytes)
                    .map_err(|_| incompatible())?
                    .trim_end_matches('\n')
                    .to_owned())
            }
            Format::HostPort if self.username.is_empty() => Ok(endpoint),
            Format::HostPort => Err(incompatible()),
            _ if self.username.is_empty() || self.username.contains(':') => Err(incompatible()),
            Format::CredentialsAt => {
                Ok(format!("{}:{}@{}", self.username, self.password, endpoint))
            }
            Format::CredentialsFirst => {
                Ok(format!("{}:{}:{}", self.username, self.password, endpoint))
            }
            Format::HostFirst => Ok(format!("{}:{}:{}", endpoint, self.username, self.password)),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct ParseError {
    pub code: &'static str,
    pub message: &'static str,
}

impl ParseError {
    fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
    fn syntax() -> Self {
        Self::new(
            "invalid_format",
            "The line does not match the selected format",
        )
    }
}

#[derive(Serialize)]
pub struct LineError {
    pub line: usize,
    #[serde(flatten)]
    pub error: ParseError,
}

pub struct ParsedImport {
    pub proxies: Vec<Proxy>,
    pub errors: Vec<LineError>,
    pub duplicates: usize,
    pub total: usize,
}

pub fn parse_import(
    text: &str,
    format: Format,
    protocol: Protocol,
) -> Result<ParsedImport, ParseError> {
    if text.len() > MAX_IMPORT_BYTES {
        return Err(ParseError::new(
            "import_too_large",
            "Import is limited to 2 MiB",
        ));
    }
    let mut parsed = ParsedImport {
        proxies: Vec::new(),
        errors: Vec::new(),
        duplicates: 0,
        total: 0,
    };
    let mut seen = HashSet::new();
    for (index, line) in text.trim_start_matches('\u{feff}').lines().enumerate() {
        if index >= MAX_IMPORT_LINES {
            return Err(ParseError::new(
                "too_many_lines",
                "Import is limited to 10000 lines",
            ));
        }
        let line = line.trim_start();
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        parsed.total += 1;
        let result = if line.len() > 4096 {
            Err(ParseError::new("line_too_long", "Line exceeds 4096 bytes"))
        } else {
            parse_line(line, format, protocol)
        };
        match result {
            Ok(proxy) if seen.insert(proxy.clone()) => parsed.proxies.push(proxy),
            Ok(_) => parsed.duplicates += 1,
            Err(error) => parsed.errors.push(LineError {
                line: index + 1,
                error,
            }),
        }
    }
    if parsed.total == 0 {
        return Err(ParseError::new("empty_import", "No proxy lines found"));
    }
    Ok(parsed)
}

fn normalize_host(host: &str) -> Result<String, ParseError> {
    let invalid = || ParseError::new("invalid_host", "Invalid IP address or hostname");
    if host.is_empty() || host.len() > 253 || host.chars().any(char::is_whitespace) {
        return Err(invalid());
    }
    let unbracketed = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if unbracketed.contains(':') {
        return unbracketed
            .parse::<Ipv6Addr>()
            .map(|ip| ip.to_string())
            .map_err(|_| invalid());
    }
    let host = url::Host::parse(host)
        .map_err(|_| invalid())?
        .to_string()
        .trim_end_matches('.')
        .to_string();
    if host.len() > 253
        || !host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'-')
        })
    {
        return Err(invalid());
    }
    Ok(host)
}

fn split_endpoint(endpoint: &str) -> Result<(&str, &str), ParseError> {
    let (host, port) = endpoint.rsplit_once(':').ok_or(ParseError::syntax())?;
    if host.contains(':') && !(host.starts_with('[') && host.ends_with(']')) {
        return Err(ParseError::syntax());
    }
    Ok((host, port))
}

fn decode_credential(value: &str) -> Result<String, ParseError> {
    let bytes = value.as_bytes();
    for (i, byte) in bytes.iter().enumerate() {
        if *byte == b'%'
            && (i + 2 >= bytes.len()
                || !bytes[i + 1].is_ascii_hexdigit()
                || !bytes[i + 2].is_ascii_hexdigit())
        {
            return Err(ParseError::new(
                "invalid_encoding",
                "Invalid percent-encoding in URL credentials",
            ));
        }
    }
    percent_decode_str(value)
        .decode_utf8()
        .map(String::from)
        .map_err(|_| ParseError::new("invalid_encoding", "URL credentials must be UTF-8"))
}

pub fn parse_line(line: &str, format: Format, protocol: Protocol) -> Result<Proxy, ParseError> {
    match format {
        Format::Auto => {
            if line.contains("://") {
                return parse_line(line, Format::Url, protocol);
            }
            let mut candidate = None;
            for format in [
                Format::Csv,
                Format::CredentialsAt,
                Format::HostPort,
                Format::HostFirst,
                Format::CredentialsFirst,
            ] {
                if let Ok(proxy) = parse_line(line, format, protocol) {
                    if candidate
                        .as_ref()
                        .is_some_and(|previous| *previous != proxy)
                    {
                        return Err(ParseError::new(
                            "ambiguous_format",
                            "Choose a format explicitly for this line",
                        ));
                    }
                    candidate = Some(proxy);
                }
            }
            candidate.ok_or(ParseError::syntax())
        }
        Format::Url | Format::HttpUrl | Format::Socks5Url => {
            let line = line.trim_end();
            let (scheme, authority) = line.split_once("://").ok_or(ParseError::syntax())?;
            let protocol = Protocol::parse(scheme)?;
            if (format == Format::HttpUrl && protocol != Protocol::Http)
                || (format == Format::Socks5Url && protocol != Protocol::Socks5)
            {
                return Err(ParseError::new(
                    "protocol_mismatch",
                    "URL protocol does not match the selected format",
                ));
            }
            if authority.contains(['/', '?', '#', '\\']) {
                return Err(ParseError::syntax());
            }
            let (credentials, endpoint) = authority
                .rsplit_once('@')
                .map_or((None, authority), |(a, b)| (Some(a), b));
            let (host, port) = split_endpoint(endpoint)?;
            let (username, password) = match credentials {
                Some(credentials) => {
                    let (u, p) = credentials.split_once(':').ok_or(ParseError::syntax())?;
                    if u.is_empty() {
                        return Err(ParseError::syntax());
                    }
                    (decode_credential(u)?, decode_credential(p)?)
                }
                None => (String::new(), String::new()),
            };
            Proxy::new(protocol, host, port, &username, &password)
        }
        Format::CredentialsAt => {
            let line = line.trim_end();
            let (credentials, endpoint) = line.rsplit_once('@').ok_or(ParseError::syntax())?;
            let (username, password) = credentials.split_once(':').ok_or(ParseError::syntax())?;
            if username.is_empty() {
                return Err(ParseError::syntax());
            }
            let (host, port) = split_endpoint(endpoint)?;
            Proxy::new(protocol, host, port, username, password)
        }
        Format::Csv => {
            let mut reader = csv::ReaderBuilder::new()
                .has_headers(false)
                .flexible(false)
                .from_reader(line.as_bytes());
            let record = reader
                .records()
                .next()
                .ok_or(ParseError::syntax())?
                .map_err(|_| ParseError::syntax())?;
            if record.len() != 4 {
                return Err(ParseError::syntax());
            }
            Proxy::new(
                protocol,
                record[0].trim(),
                record[1].trim(),
                &record[2],
                &record[3],
            )
        }
        Format::CredentialsFirst => {
            let line = line.trim_end();
            let (username, rest) = line.split_once(':').ok_or(ParseError::syntax())?;
            let (before_port, port) = rest.rsplit_once(':').ok_or(ParseError::syntax())?;
            let (password, host) = if before_port.ends_with(']') {
                let index = before_port.rfind(":[").ok_or(ParseError::syntax())?;
                (&before_port[..index], &before_port[index + 1..])
            } else {
                before_port.rsplit_once(':').ok_or(ParseError::syntax())?
            };
            if username.is_empty() {
                return Err(ParseError::syntax());
            }
            Proxy::new(protocol, host, port, username, password)
        }
        Format::HostFirst => {
            let (host, rest) = if line.starts_with('[') {
                let index = line.find("]:").ok_or(ParseError::syntax())?;
                (&line[..index + 1], &line[index + 2..])
            } else {
                line.split_once(':').ok_or(ParseError::syntax())?
            };
            let (port, credentials) = rest.split_once(':').ok_or(ParseError::syntax())?;
            let (username, password) = credentials.split_once(':').ok_or(ParseError::syntax())?;
            if username.is_empty() {
                return Err(ParseError::syntax());
            }
            Proxy::new(protocol, host, port, username, password)
        }
        Format::HostPort => {
            let (host, port) = split_endpoint(line.trim_end())?;
            Proxy::new(protocol, host, port, "", "")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_password_spaces_and_detects_delimiter_ambiguity() {
        for (text, format) in [
            ("proxy.example:80:alice: secret ", Format::HostFirst),
            ("proxy.example,80,alice, secret ", Format::Csv),
            ("http://alice:%20secret%20@proxy.example:80", Format::Url),
        ] {
            let parsed = parse_import(text, format, Protocol::Http).unwrap();
            assert_eq!(parsed.proxies[0].password, " secret ");
            let csv = parsed.proxies[0].export(Format::Csv).unwrap();
            assert_eq!(
                parse_import(&csv, Format::Csv, Protocol::Http)
                    .unwrap()
                    .proxies[0]
                    .password,
                " secret "
            );
        }
        assert!(
            parse_line(
                "proxy.example:80:user:p@other.example:90",
                Format::Auto,
                Protocol::Http
            )
            .is_err()
        );
        let parsed =
            parse_line("proxy.example:80:user:p@ss", Format::Auto, Protocol::Http).unwrap();
        assert_eq!(parsed.password, "p@ss");
    }

    #[test]
    fn supports_all_screenshot_formats() {
        for (line, protocol) in [
            ("alice:secret@proxy.example:10000", Protocol::Http),
            ("proxy.example,10000,alice,secret", Protocol::Http),
            ("http://alice:secret@proxy.example:10000", Protocol::Http),
            (
                "socks5://alice:secret@proxy.example:10000",
                Protocol::Socks5,
            ),
            ("alice:secret:proxy.example:10000", Protocol::Http),
            ("proxy.example:10000:alice:secret", Protocol::Http),
        ] {
            let p = parse_line(line, Format::Auto, Protocol::Http).unwrap();
            assert_eq!(
                (
                    p.host.as_str(),
                    p.port,
                    p.username.as_str(),
                    p.password.as_str(),
                    p.protocol
                ),
                ("proxy.example", 10000, "alice", "secret", protocol)
            );
        }
    }

    #[test]
    fn roundtrips_ipv6_and_special_credentials() {
        for host in ["proxy.example", "[2001:db8::1]"] {
            let p = Proxy::new(Protocol::Socks5, host, "1080", "alice", "p:a@ss,word%20").unwrap();
            for format in [
                Format::Url,
                Format::CredentialsAt,
                Format::Csv,
                Format::CredentialsFirst,
                Format::HostFirst,
            ] {
                let exported = p.export(format).unwrap();
                assert!(p == parse_line(&exported, format, Protocol::Socks5).unwrap());
            }
        }
        let p = parse_line(
            "https://u%3An:p%40ss%3A%25@proxy.example:443",
            Format::Auto,
            Protocol::Http,
        )
        .unwrap();
        assert_eq!(
            (p.username.as_str(), p.password.as_str(), p.port),
            ("u:n", "p@ss:%", 443)
        );
        assert!(p.export(Format::CredentialsAt).is_err());
        assert!(
            p == parse_line(
                &p.export(Format::Url).unwrap(),
                Format::Auto,
                Protocol::Http
            )
            .unwrap()
        );
    }

    #[test]
    fn rejects_ambiguous_lines_and_invalid_urls() {
        assert_eq!(
            parse_line("one:1234:two:5678", Format::Auto, Protocol::Http)
                .err()
                .unwrap()
                .code,
            "ambiguous_format"
        );
        assert!(parse_line("one:1234:two:5678", Format::HostFirst, Protocol::Http).is_ok());
        for line in [
            "http://proxy.example",
            "http://proxy.example:0",
            "http://proxy.example:65536",
            "ftp://proxy.example:21",
            "http://u:p%0A@proxy.example:80",
            "http://u:p%ZZ@proxy.example:80",
            "http://proxy.example:80/path",
            "http://proxy.example:80?x=1",
            "http://bad host:80",
            "http://u:p@:80",
            "http://2001:db8::1:80",
        ] {
            assert!(
                parse_line(line, Format::Auto, Protocol::Http).is_err(),
                "Unexpectedly accepted {line}"
            );
        }
    }

    #[test]
    fn import_tracks_line_numbers_and_canonical_duplicates() {
        let p = parse_import("\u{feff}# example\r\n\r\nhttp://u:p@PROXY.example:80\r\nu:p@proxy.example:80\r\ninvalid", Format::Auto, Protocol::Http).unwrap();
        assert_eq!(
            (
                p.total,
                p.proxies.len(),
                p.duplicates,
                p.errors.len(),
                p.errors[0].line
            ),
            (3, 1, 1, 1, 5)
        );
    }

    #[test]
    fn bounds_imports_and_honors_default_protocol() {
        assert!(
            parse_import(
                &"x".repeat(MAX_IMPORT_BYTES + 1),
                Format::Auto,
                Protocol::Http
            )
            .is_err()
        );
        assert!(
            parse_import(
                &"x\n".repeat(MAX_IMPORT_LINES + 1),
                Format::Auto,
                Protocol::Http
            )
            .is_err()
        );
        assert!(parse_import("\n# only comment", Format::Auto, Protocol::Http).is_err());
        let p = parse_line("127.0.0.1:1080", Format::Auto, Protocol::Socks5).unwrap();
        assert_eq!(p.protocol, Protocol::Socks5);
        assert_eq!(p.export(Format::Url).unwrap(), "socks5://127.0.0.1:1080");
        assert!(p.export(Format::HttpUrl).is_err());
    }
}
