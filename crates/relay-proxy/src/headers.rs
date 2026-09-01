use std::{net::IpAddr, str::FromStr as _};

use http::{HeaderMap, HeaderName, HeaderValue, header};
use thiserror::Error;

const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");
const X_FORWARDED_HOST: HeaderName = HeaderName::from_static("x-forwarded-host");
const X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");
const X_FORWARDED_PORT: HeaderName = HeaderName::from_static("x-forwarded-port");
const FORWARDED: HeaderName = HeaderName::from_static("forwarded");
const VIA: HeaderName = HeaderName::from_static("via");

/// Rewrites request headers at the downstream-to-upstream trust boundary.
pub(crate) fn prepare_upstream_request(
    headers: &mut HeaderMap,
    authority: &str,
    client_ip: IpAddr,
    protocol: &'static str,
) -> Result<(), HeaderRewriteError> {
    let original_host = headers.get(header::HOST).cloned();
    let accepts_trailers = headers.get_all(header::TE).iter().any(|value| {
        value.to_str().is_ok_and(|value| {
            value
                .split(',')
                .map(str::trim)
                .any(|token| token.eq_ignore_ascii_case("trailers"))
        })
    });
    remove_hop_by_hop(headers);
    for untrusted in [
        FORWARDED,
        X_FORWARDED_FOR,
        X_FORWARDED_HOST,
        X_FORWARDED_PROTO,
        X_FORWARDED_PORT,
    ] {
        headers.remove(untrusted);
    }
    headers.insert(header::HOST, HeaderValue::from_str(authority)?);
    headers.insert(
        X_FORWARDED_FOR,
        HeaderValue::from_str(&client_ip.to_string())?,
    );
    headers.insert(X_FORWARDED_PROTO, HeaderValue::from_static(protocol));
    if accepts_trailers {
        headers.insert(header::TE, HeaderValue::from_static("trailers"));
    }
    headers.insert(VIA, HeaderValue::from_static("1.1 relay"));
    if let Some(original_host) = original_host {
        headers.insert(X_FORWARDED_HOST, original_host);
    }
    Ok(())
}

/// Removes headers that apply only to one transport connection.
pub(crate) fn prepare_downstream_response(headers: &mut HeaderMap) {
    remove_hop_by_hop(headers);
    headers.insert(VIA, HeaderValue::from_static("1.1 relay"));
}

fn remove_hop_by_hop(headers: &mut HeaderMap) {
    let connection_headers = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::from_str(name.trim()).ok())
        .collect::<Vec<_>>();
    for name in connection_headers {
        headers.remove(name);
    }
    for name in [
        header::CONNECTION,
        HeaderName::from_static("keep-alive"),
        header::PROXY_AUTHENTICATE,
        header::PROXY_AUTHORIZATION,
        header::TE,
        header::TRAILER,
        header::TRANSFER_ENCODING,
        header::UPGRADE,
    ] {
        headers.remove(name);
    }
}

/// A generated forwarding header could not be represented safely.
#[derive(Debug, Error)]
#[error("generated forwarding header is invalid: {0}")]
pub(crate) struct HeaderRewriteError(#[from] http::header::InvalidHeaderValue);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_nominated_and_standard_hop_headers_are_removed() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONNECTION,
            HeaderValue::from_static("keep-alive, x-hop"),
        );
        headers.insert("keep-alive", HeaderValue::from_static("timeout=5"));
        headers.insert("x-hop", HeaderValue::from_static("private"));
        headers.insert("x-end-to-end", HeaderValue::from_static("preserved"));

        prepare_downstream_response(&mut headers);

        assert!(!headers.contains_key(header::CONNECTION));
        assert!(!headers.contains_key("keep-alive"));
        assert!(!headers.contains_key("x-hop"));
        assert_eq!(headers["x-end-to-end"], "preserved");
        assert_eq!(headers[VIA], "1.1 relay");
    }

    #[test]
    fn untrusted_forwarding_headers_are_replaced_at_the_edge() {
        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("public.example"));
        headers.insert(X_FORWARDED_FOR, HeaderValue::from_static("203.0.113.99"));
        headers.insert(
            FORWARDED,
            HeaderValue::from_static("for=203.0.113.99;proto=https"),
        );
        headers.insert(X_FORWARDED_PORT, HeaderValue::from_static("444"));

        prepare_upstream_request(
            &mut headers,
            "127.0.0.1:9001",
            "192.0.2.10".parse().expect("valid IP"),
            "https",
        )
        .expect("generated values should be valid");

        assert_eq!(headers[header::HOST], "127.0.0.1:9001");
        assert_eq!(headers[X_FORWARDED_HOST], "public.example");
        assert_eq!(headers[X_FORWARDED_FOR], "192.0.2.10");
        assert_eq!(headers[X_FORWARDED_PROTO], "https");
        assert!(!headers.contains_key(FORWARDED));
        assert!(!headers.contains_key(X_FORWARDED_PORT));
    }

    #[test]
    fn grpc_trailer_capability_survives_the_hop_boundary() {
        let mut headers = HeaderMap::new();
        headers.insert(header::TE, HeaderValue::from_static("trailers"));

        prepare_upstream_request(
            &mut headers,
            "127.0.0.1:9001",
            "192.0.2.10".parse().expect("valid IP"),
            "https",
        )
        .expect("generated values should be valid");

        assert_eq!(headers[header::TE], "trailers");
    }
}
