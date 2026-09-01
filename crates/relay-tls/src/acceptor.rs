use std::{
    fs::File,
    io::BufReader,
    path::{Path, PathBuf},
    sync::Arc,
};

use arc_swap::ArcSwap;
use relay_config::TlsConfig;
use rustls::{ServerConfig, pki_types::PrivateKeyDer};
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_rustls::{TlsAcceptor, server::TlsStream};

/// A TLS acceptor whose certificate identity can be replaced atomically.
#[derive(Clone)]
pub struct ReloadingTlsAcceptor {
    active: Arc<ArcSwap<ServerConfig>>,
}

impl ReloadingTlsAcceptor {
    /// Loads the initial certificate chain and private key.
    pub fn from_files(config: &TlsConfig) -> Result<Self, TlsLoadError> {
        Ok(Self {
            active: Arc::new(ArcSwap::from(load_server_config(config)?)),
        })
    }

    /// Completes a server-side TLS handshake with the currently installed identity.
    pub async fn accept(&self, stream: TcpStream) -> Result<TlsStream<TcpStream>, std::io::Error> {
        TlsAcceptor::from(self.active.load_full())
            .accept(stream)
            .await
    }

    /// Validates and atomically replaces the certificate identity for new connections.
    pub fn reload(&self, config: &TlsConfig) -> Result<(), TlsLoadError> {
        self.active.store(load_server_config(config)?);
        Ok(())
    }
}

impl std::fmt::Debug for ReloadingTlsAcceptor {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReloadingTlsAcceptor")
            .finish_non_exhaustive()
    }
}

fn load_server_config(config: &TlsConfig) -> Result<Arc<ServerConfig>, TlsLoadError> {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    let certificate_path = PathBuf::from(&config.certificate);
    let key_path = PathBuf::from(&config.private_key);
    let mut certificate_reader = BufReader::new(open(&certificate_path)?);
    let certificates = rustls_pemfile::certs(&mut certificate_reader)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| TlsLoadError::Pem {
            path: certificate_path.clone(),
            source,
        })?;
    if certificates.is_empty() {
        return Err(TlsLoadError::EmptyCertificateChain {
            path: certificate_path,
        });
    }
    let mut key_reader = BufReader::new(open(&key_path)?);
    let private_key = rustls_pemfile::private_key(&mut key_reader)
        .map_err(|source| TlsLoadError::Pem {
            path: key_path.clone(),
            source,
        })?
        .ok_or_else(|| TlsLoadError::MissingPrivateKey {
            path: key_path.clone(),
        })?;
    build_config(certificates, private_key, &key_path)
}

fn build_config(
    certificates: Vec<rustls::pki_types::CertificateDer<'static>>,
    private_key: PrivateKeyDer<'static>,
    key_path: &Path,
) -> Result<Arc<ServerConfig>, TlsLoadError> {
    let mut config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .map_err(|source| TlsLoadError::Identity {
            path: key_path.to_path_buf(),
            source,
        })?;
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

fn open(path: &Path) -> Result<File, TlsLoadError> {
    File::open(path).map_err(|source| TlsLoadError::Read {
        path: path.to_path_buf(),
        source,
    })
}

/// Certificate, key, or identity configuration failure.
#[derive(Debug, Error)]
pub enum TlsLoadError {
    /// A PEM file could not be opened.
    #[error("failed to read TLS file {}: {source}", path.display())]
    Read {
        /// File path.
        path: PathBuf,
        /// Filesystem error.
        source: std::io::Error,
    },
    /// A PEM item could not be decoded.
    #[error("failed to decode TLS PEM {}: {source}", path.display())]
    Pem {
        /// File path.
        path: PathBuf,
        /// PEM decoding error.
        source: std::io::Error,
    },
    /// The certificate file contained no certificates.
    #[error("TLS certificate file {} contains no certificates", path.display())]
    EmptyCertificateChain {
        /// Certificate path.
        path: PathBuf,
    },
    /// The key file contained no supported private key.
    #[error("TLS key file {} contains no private key", path.display())]
    MissingPrivateKey {
        /// Key path.
        path: PathBuf,
    },
    /// The private key does not match the certificate or is unsupported.
    #[error("TLS identity using key {} is invalid: {source}", path.display())]
    Identity {
        /// Key path.
        path: PathBuf,
        /// rustls validation error.
        source: rustls::Error,
    },
}
