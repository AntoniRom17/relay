use std::{
    fs::File,
    path::{Path, PathBuf},
    sync::Arc,
};

use arc_swap::ArcSwap;
use relay_config::TlsConfig;
use rustls::{
    ServerConfig,
    pki_types::{
        CertificateDer, PrivateKeyDer,
        pem::{Error as PemError, PemObject},
    },
};
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
    ///
    /// # Errors
    ///
    /// Returns [`TlsLoadError`] when a configured PEM file cannot be read or parsed, or the key
    /// does not match the certificate chain.
    pub fn from_files(config: &TlsConfig) -> Result<Self, TlsLoadError> {
        Ok(Self {
            active: Arc::new(ArcSwap::from(load_server_config(config)?)),
        })
    }

    /// Completes a server-side TLS handshake with the currently installed identity.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the peer disconnects or the TLS handshake fails.
    pub async fn accept(&self, stream: TcpStream) -> Result<TlsStream<TcpStream>, std::io::Error> {
        TlsAcceptor::from(self.active.load_full())
            .accept(stream)
            .await
    }

    /// Validates and atomically replaces the certificate identity for new connections.
    ///
    /// # Errors
    ///
    /// Returns [`TlsLoadError`] without changing the active identity when replacement material is
    /// unreadable or invalid.
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
    let certificate_reader = open(&certificate_path)?;
    let certificates = CertificateDer::pem_reader_iter(certificate_reader)
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
    let key_reader = open(&key_path)?;
    let private_key = match PrivateKeyDer::from_pem_reader(key_reader) {
        Ok(private_key) => private_key,
        Err(PemError::NoItemsFound) => {
            return Err(TlsLoadError::MissingPrivateKey {
                path: key_path.clone(),
            });
        }
        Err(source) => {
            return Err(TlsLoadError::Pem {
                path: key_path.clone(),
                source,
            });
        }
    };
    build_config(certificates, private_key, &key_path)
}

fn build_config(
    certificates: Vec<CertificateDer<'static>>,
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
        source: PemError,
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
