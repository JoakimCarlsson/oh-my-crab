//! AWS Signature Version 4: the headers that sign one HTTP request with an access key.

use ring::hmac;
use sha2::{Digest, Sha256};

/// The algorithm a signature names.
const ALGORITHM: &str = "AWS4-HMAC-SHA256";

/// An AWS access key, with the session token temporary credentials come with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Keys {
    /// The access key id, such as `AKIA...`.
    pub access_key: String,
    /// The secret that signs.
    pub secret_key: String,
    /// The session token of temporary credentials, sent as `x-amz-security-token`.
    pub session_token: Option<String>,
}

/// One request to sign.
pub(crate) struct Signing<'a> {
    /// The HTTP method, such as `POST`.
    pub method: &'a str,
    /// The `Host` header the request is sent with: the host, and the port when it is not the scheme's own.
    pub host: &'a str,
    /// The path as it is sent, already percent-encoded.
    pub path: &'a str,
    /// The query parameters, not encoded.
    pub query: &'a [(&'a str, &'a str)],
    /// Headers signed besides `host`, `x-amz-date` and `x-amz-security-token`, as they are sent.
    pub headers: &'a [(&'a str, &'a str)],
    /// The body.
    pub payload: &'a [u8],
    /// The region, such as `us-east-1`.
    pub region: &'a str,
    /// The service's signing name, such as `bedrock`.
    pub service: &'a str,
    /// The time of signing as `YYYYMMDDTHHMMSSZ`.
    pub amz_date: &'a str,
}

/// The headers to add to the request `signing` describes so that AWS accepts it as signed with `keys`: `x-amz-date`, `x-amz-security-token` when there is a session token, and `authorization`.
pub(crate) fn sign(keys: &Keys, signing: &Signing<'_>) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = signing
        .headers
        .iter()
        .map(|(name, value)| (name.to_ascii_lowercase(), trim_value(value)))
        .collect();
    headers.push(("host".to_owned(), signing.host.to_owned()));
    headers.push(("x-amz-date".to_owned(), signing.amz_date.to_owned()));
    if let Some(token) = &keys.session_token {
        headers.push(("x-amz-security-token".to_owned(), token.clone()));
    }
    headers.sort();
    let signed: Vec<&str> = headers.iter().map(|(name, _)| name.as_str()).collect();
    let signed = signed.join(";");
    let canonical = canonical_request(signing, &headers, &signed);
    let date = &signing.amz_date[..signing.amz_date.len().min(8)];
    let scope = format!("{date}/{}/{}/aws4_request", signing.region, signing.service);
    let to_sign = format!(
        "{ALGORITHM}\n{}\n{scope}\n{}",
        signing.amz_date,
        hex(&Sha256::digest(canonical.as_bytes()))
    );
    let key = signing_key(&keys.secret_key, date, signing.region, signing.service);
    let signature = hex(hmac::sign(&key, to_sign.as_bytes()).as_ref());
    let mut added = vec![("x-amz-date".to_owned(), signing.amz_date.to_owned())];
    if let Some(token) = &keys.session_token {
        added.push(("x-amz-security-token".to_owned(), token.clone()));
    }
    added.push((
        "authorization".to_owned(),
        format!(
            "{ALGORITHM} Credential={}/{scope}, SignedHeaders={signed}, Signature={signature}",
            keys.access_key
        ),
    ));
    added
}

/// The canonical request of `signing` with its lower-cased, sorted `headers`, whose names joined are `signed`.
fn canonical_request(signing: &Signing<'_>, headers: &[(String, String)], signed: &str) -> String {
    let mut query: Vec<(String, String)> = signing
        .query
        .iter()
        .map(|(name, value)| (uri_encode(name, true), uri_encode(value, true)))
        .collect();
    query.sort();
    let query: Vec<String> = query
        .into_iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect();
    let headers: String = headers
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect();
    format!(
        "{}\n{}\n{}\n{headers}\n{signed}\n{}",
        signing.method,
        canonical_uri(signing.path),
        query.join("&"),
        hex(&Sha256::digest(signing.payload))
    )
}

/// The canonical form of `path` as sent: every segment percent-encoded once more, as every service but S3 wants, so `%3A` becomes `%253A`.
fn canonical_uri(path: &str) -> String {
    if path.is_empty() {
        return "/".to_owned();
    }
    path.split('/')
        .map(|segment| uri_encode(segment, true))
        .collect::<Vec<_>>()
        .join("/")
}

/// `text` percent-encoded as SigV4 wants: every byte but the unreserved letters, digits, `-`, `_`, `.` and `~` as `%XX` in upper case, and `/` too when `slash` says so.
pub(crate) fn uri_encode(text: &str, slash: bool) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(byte, b'-' | b'_' | b'.' | b'~')
            || (byte == b'/' && !slash);
        if keep {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// `value` with the spaces around it dropped and every run of spaces inside it made one.
fn trim_value(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The key that signs requests made on `date` (`YYYYMMDD`) to `service` in `region` with `secret`.
fn signing_key(secret: &str, date: &str, region: &str, service: &str) -> hmac::Key {
    let step = |key: &[u8], data: &str| {
        hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, key), data.as_bytes())
    };
    let date_key = step(format!("AWS4{secret}").as_bytes(), date);
    let region_key = step(date_key.as_ref(), region);
    let service_key = step(region_key.as_ref(), service);
    let signing = step(service_key.as_ref(), "aws4_request");
    hmac::Key::new(hmac::HMAC_SHA256, signing.as_ref())
}

/// `bytes` as lower-case hexadecimal.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The UTC time `seconds` after the Unix epoch as `YYYYMMDDTHHMMSSZ`.
pub(crate) fn amz_date(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    let (year, month, day) = civil(days);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The proleptic Gregorian year, month and day of the day `days` after 1970-01-01.
fn civil(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// The current time as `YYYYMMDDTHHMMSSZ`.
pub(crate) fn now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    amz_date(seconds)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The example key of AWS's signature test suite.
    fn example_keys() -> Keys {
        Keys {
            access_key: "AKIDEXAMPLE".to_owned(),
            secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".to_owned(),
            session_token: None,
        }
    }

    /// The request of AWS's test suite at `/` with `method` and `query`.
    fn suite_request<'a>(method: &'a str, query: &'a [(&'a str, &'a str)]) -> Signing<'a> {
        Signing {
            method,
            host: "example.amazonaws.com",
            path: "/",
            query,
            headers: &[],
            payload: b"",
            region: "us-east-1",
            service: "service",
            amz_date: "20150830T123600Z",
        }
    }

    /// The `authorization` header among `headers`.
    fn authorization(headers: &[(String, String)]) -> &str {
        headers
            .iter()
            .find(|(name, _)| name == "authorization")
            .map(|(_, value)| value.as_str())
            .unwrap()
    }

    /// The signing key matches the one AWS's documentation derives for its example secret.
    #[test]
    fn derives_the_documented_signing_key() {
        let key = signing_key(
            "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "20120215",
            "us-east-1",
            "iam",
        );
        let probe = hmac::sign(&key, b"");
        let expected = hmac::sign(
            &hmac::Key::new(
                hmac::HMAC_SHA256,
                &(0..32)
                    .map(|at| {
                        u8::from_str_radix(
                            &"f4780e2d9f65fa895f9c67b32ce1baf0b0d8a43505a000a1a9e090d414db404d"
                                [at * 2..at * 2 + 2],
                            16,
                        )
                        .unwrap()
                    })
                    .collect::<Vec<u8>>(),
            ),
            b"",
        );
        assert_eq!(probe.as_ref(), expected.as_ref());
    }

    /// `get-vanilla` of AWS's signature test suite.
    #[test]
    fn signs_get_vanilla() {
        let headers = sign(&example_keys(), &suite_request("GET", &[]));
        assert_eq!(
            authorization(&headers),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31"
        );
        assert_eq!(headers[0], ("x-amz-date".into(), "20150830T123600Z".into()));
    }

    /// `post-vanilla` of AWS's signature test suite.
    #[test]
    fn signs_post_vanilla() {
        let headers = sign(&example_keys(), &suite_request("POST", &[]));
        assert!(authorization(&headers).ends_with(
            "Signature=5da7c1a2acd57cee7505fc6676e4e544621c30862966e37dddb68e92efbe5d6b"
        ));
    }

    /// `get-vanilla-query-order-key-case` of AWS's signature test suite: parameters are signed in sorted order.
    #[test]
    fn signs_sorted_query_parameters() {
        let query = [("Param2", "value2"), ("Param1", "value1")];
        let headers = sign(&example_keys(), &suite_request("GET", &query));
        assert!(authorization(&headers).ends_with(
            "Signature=b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500"
        ));
    }

    /// The IAM `ListUsers` example of AWS's documentation, which signs a content type too.
    #[test]
    fn signs_the_documented_iam_example() {
        let signing = Signing {
            method: "GET",
            host: "iam.amazonaws.com",
            path: "/",
            query: &[("Action", "ListUsers"), ("Version", "2010-05-08")],
            headers: &[(
                "Content-Type",
                "application/x-www-form-urlencoded; charset=utf-8",
            )],
            payload: b"",
            region: "us-east-1",
            service: "iam",
            amz_date: "20150830T123600Z",
        };
        let headers = sign(&example_keys(), &signing);
        assert_eq!(
            authorization(&headers),
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/iam/aws4_request, SignedHeaders=content-type;host;x-amz-date, Signature=5d672d79c15b13162d9279b0855cfba6789a8edb4c82c400e06b5924a6f2b5d7"
        );
    }

    /// A session token is sent and signed, and a path is encoded twice for signing.
    #[test]
    fn signs_session_tokens_and_encoded_paths() {
        let keys = Keys {
            session_token: Some("token-1".to_owned()),
            ..example_keys()
        };
        let mut signing = suite_request("POST", &[]);
        signing.path = "/model/anthropic.claude-v2%3A1/invoke";
        let headers = sign(&keys, &signing);
        assert_eq!(
            headers[1],
            ("x-amz-security-token".into(), "token-1".into())
        );
        assert!(
            authorization(&headers).contains("SignedHeaders=host;x-amz-date;x-amz-security-token,")
        );
        assert_eq!(
            canonical_uri("/model/anthropic.claude-v2%3A1/invoke"),
            "/model/anthropic.claude-v2%253A1/invoke"
        );
        assert_eq!(uri_encode("a b/c~", true), "a%20b%2Fc~");
        assert_eq!(uri_encode("a/b", false), "a/b");
    }

    /// Unix times format as the UTC dates they are.
    #[test]
    fn formats_amz_dates() {
        assert_eq!(amz_date(0), "19700101T000000Z");
        assert_eq!(amz_date(1_440_938_160), "20150830T123600Z");
        assert_eq!(amz_date(951_782_400), "20000229T000000Z");
        assert_eq!(amz_date(1_794_072_723), "20261107T173203Z");
    }
}
