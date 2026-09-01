use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use http::{Request, Uri};
use http_body_util::{BodyExt as _, Empty};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{Client, connect::HttpConnector},
    rt::TokioExecutor,
};
use relay_upstream::HealthProbe;
use url::Url;

/// HTTP/HTTPS active health probe backed by Hyper's pooled client.
#[derive(Clone)]
pub struct HyperHealthProbe {
    client: Client<HttpsConnector<HttpConnector>, Empty<Bytes>>,
}

impl HyperHealthProbe {
    /// Creates a probe client with HTTP/1.1 and HTTP/2 ALPN support.
    pub fn new(connect_timeout: Duration) -> Self {
        let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
        let mut connector = HttpConnector::new();
        connector.enforce_http(false);
        connector.set_connect_timeout(Some(connect_timeout));
        let https = HttpsConnectorBuilder::new()
            .with_webpki_roots()
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .wrap_connector(connector);
        Self {
            client: Client::builder(TokioExecutor::new())
                .pool_idle_timeout(Duration::from_secs(30))
                .pool_max_idle_per_host(4)
                .build(https),
        }
    }
}

impl std::fmt::Debug for HyperHealthProbe {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HyperHealthProbe")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl HealthProbe for HyperHealthProbe {
    async fn probe(
        &self,
        endpoint: &Url,
        path: &str,
        timeout: Duration,
        accepted_status: std::ops::RangeInclusive<u16>,
    ) -> bool {
        let Ok(uri) = health_uri(endpoint, path) else {
            return false;
        };
        let Ok(request) = Request::get(uri).body(Empty::new()) else {
            return false;
        };
        let operation = async {
            let mut response = self.client.request(request).await.ok()?;
            let status = response.status().as_u16();
            while let Some(frame) = response.body_mut().frame().await {
                frame.ok()?;
            }
            Some(accepted_status.contains(&status))
        };
        tokio::time::timeout(timeout, operation)
            .await
            .ok()
            .flatten()
            .unwrap_or(false)
    }
}

fn health_uri(endpoint: &Url, path: &str) -> Result<Uri, http::uri::InvalidUri> {
    let origin = endpoint.origin().ascii_serialization();
    let base = endpoint.path().trim_end_matches('/');
    let path = if path.starts_with('/') {
        path.to_owned()
    } else {
        format!("/{path}")
    };
    format!("{origin}{base}{path}").parse()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn health_path_is_joined_beneath_endpoint_base_path() {
        let endpoint: Url = "https://api.example/base/".parse().expect("URL");

        assert_eq!(
            health_uri(&endpoint, "/ready").expect("URI"),
            "https://api.example/base/ready"
        );
    }
}
