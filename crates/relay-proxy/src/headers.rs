use std::{net::IpAddr, str::FromStr as _};

use http::{HeaderMap, HeaderName, HeaderValue, header};
use thiserror::Error;

const X_FORWARDED_FOR: HeaderName = HeaderName::from_static("x-forwarded-for");
const X_FORWARDED_HOST: HeaderName = HeaderName::from_static("x-forwarded-host");
const X_FORWARDED_PROTO: HeaderName = HeaderName::from_static("x-forwarded-proto");
const VIA: HeaderName = HeaderName::from_static("via");

/// Rewrites request headers at the downstream-to-upstream trust boundary.
pub(crate) fn prepare_upstream_request(
    headers: &mut HeaderMap,
    authority: &str,
    client_ip: IpAddr,
    protocol: &'static str,
) -> Result<(), HeaderRewriteError> {
    let original_host = headers.get(header::HOST).cloned();
    remove_hop_by_hop(headers);
    headers.insert(header::HOST, HeaderValue::from_str(authority)?);
    headers.insert(
        X_FORWARDED_FOR,
        HeaderValue::from_str(&client_ip.to_string())?,
    );
    headers.insert(X_FORWARDED_PROTO, HeaderValue::from_static(protocol));
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
