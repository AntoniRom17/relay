use std::{collections::BTreeMap, str::FromStr as _};

use http::{HeaderMap, HeaderName, header};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode};
use relay_config::{AuthPolicyConfig, AuthenticationConfig, JwtAlgorithm, JwtProviderConfig};
use serde_json::Value;
use sha2::{Digest as _, Sha256};
use subtle::ConstantTimeEq as _;
use thiserror::Error;

/// Immutable authentication material compiled at startup or reload.
pub struct Authenticator {
    api_keys: Box<[[u8; 32]]>,
    jwt_providers: BTreeMap<String, JwtProvider>,
}

struct JwtProvider {
    key: DecodingKey,
    validation: Validation,
}

impl Authenticator {
    /// Compiles configured key digests and JWT public keys.
    pub fn from_config(config: &AuthenticationConfig) -> Result<Self, AuthBuildError> {
        let api_keys = config
            .api_key_sha256
            .iter()
            .map(|digest| {
                let decoded = hex::decode(digest)
                    .map_err(|source| AuthBuildError::ApiKeyDigest { source })?;
                decoded
                    .try_into()
                    .map_err(|_| AuthBuildError::ApiKeyDigestLength)
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice();
        let jwt_providers = config
            .jwt_providers
            .iter()
            .map(|(name, provider)| {
                compile_provider(provider)
                    .map(|provider| (name.clone(), provider))
                    .map_err(|source| AuthBuildError::JwtProvider {
                        provider: name.clone(),
                        source,
                    })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            api_keys,
            jwt_providers,
        })
    }

    /// Enforces a route policy and returns non-secret identity context.
    pub fn authenticate(
        &self,
        headers: &HeaderMap,
        policy: &AuthPolicyConfig,
    ) -> Result<AuthIdentity, AuthError> {
        match policy {
            AuthPolicyConfig::ApiKey { header } => self.authenticate_api_key(headers, header),
            AuthPolicyConfig::Jwt { provider } => self.authenticate_jwt(headers, provider),
        }
    }

    fn authenticate_api_key(
        &self,
        headers: &HeaderMap,
        header_name: &str,
    ) -> Result<AuthIdentity, AuthError> {
        let header = HeaderName::from_str(header_name).map_err(|_| AuthError::InvalidCredential)?;
        let presented = headers.get(header).ok_or(AuthError::MissingCredential)?;
        let digest: [u8; 32] = Sha256::digest(presented.as_bytes()).into();
        let mut matched = 0_u8;
        let mut fingerprint = None;
        for configured in &self.api_keys {
            let equal = digest.ct_eq(configured).unwrap_u8();
            matched |= equal;
            if equal == 1 {
                fingerprint = Some(hex::encode(&configured[..4]));
            }
        }
        if matched == 0 {
            return Err(AuthError::InvalidCredential);
        }
        Ok(AuthIdentity {
            subject: None,
            api_key_fingerprint: fingerprint,
            claims: None,
        })
    }

    fn authenticate_jwt(
        &self,
        headers: &HeaderMap,
        provider: &str,
    ) -> Result<AuthIdentity, AuthError> {
        let provider = self
            .jwt_providers
            .get(provider)
            .ok_or(AuthError::InvalidCredential)?;
        let authorization = headers
            .get(header::AUTHORIZATION)
            .ok_or(AuthError::MissingCredential)?
            .to_str()
            .map_err(|_| AuthError::InvalidCredential)?;
        let token = authorization
            .strip_prefix("Bearer ")
            .ok_or(AuthError::InvalidCredential)?;
        let data = decode::<Value>(token, &provider.key, &provider.validation)
            .map_err(|_| AuthError::InvalidCredential)?;
        let subject = data
            .claims
            .get("sub")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(AuthIdentity {
            subject,
            api_key_fingerprint: None,
            claims: Some(data.claims),
        })
    }
}

impl std::fmt::Debug for Authenticator {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Authenticator")
            .field("api_key_count", &self.api_keys.len())
            .field("jwt_provider_count", &self.jwt_providers.len())
            .finish()
    }
}

fn compile_provider(
    config: &JwtProviderConfig,
) -> Result<JwtProvider, jsonwebtoken::errors::Error> {
    let (algorithm, key) = match config.algorithm {
        JwtAlgorithm::RS256 => (
            Algorithm::RS256,
            DecodingKey::from_rsa_pem(config.public_key.as_bytes())?,
        ),
        JwtAlgorithm::ES256 => (
            Algorithm::ES256,
            DecodingKey::from_ec_pem(config.public_key.as_bytes())?,
        ),
        JwtAlgorithm::EdDSA => (
            Algorithm::EdDSA,
            DecodingKey::from_ed_pem(config.public_key.as_bytes())?,
        ),
    };
    let mut validation = Validation::new(algorithm);
    validation.set_issuer(&[config.issuer.as_str()]);
    validation.set_audience(&[config.audience.as_str()]);
    validation.set_required_spec_claims(&["exp", "iss", "aud"]);
    validation.validate_exp = true;
    Ok(JwtProvider { key, validation })
}

/// Identity attributes safe to attach to request context and spans.
#[derive(Debug, Clone)]
pub struct AuthIdentity {
    /// JWT subject, when present.
    pub subject: Option<String>,
    /// First four bytes of the matched API-key digest.
    pub api_key_fingerprint: Option<String>,
    /// Validated JWT claims for downstream policy use.
    pub claims: Option<Value>,
}

/// Authentication material could not be compiled.
#[derive(Debug, Error)]
pub enum AuthBuildError {
    /// An API key digest is not hexadecimal.
    #[error("API key SHA-256 digest is invalid: {source}")]
    ApiKeyDigest {
        /// Hex decoding error.
        source: hex::FromHexError,
    },
    /// A decoded API key digest is not 32 bytes.
    #[error("API key SHA-256 digest must decode to exactly 32 bytes")]
    ApiKeyDigestLength,
    /// A named JWT provider contains unusable key material.
    #[error("JWT provider `{provider}` is invalid: {source}")]
    JwtProvider {
        /// Provider name.
        provider: String,
        /// Key parsing or validation construction error.
        source: jsonwebtoken::errors::Error,
    },
}

/// Request credentials are absent or invalid.
#[derive(Debug, Clone, Copy, Error, PartialEq, Eq)]
pub enum AuthError {
    /// The route requires a credential that was not provided.
    #[error("required authentication credential is missing")]
    MissingCredential,
    /// A provided credential failed validation.
    #[error("authentication credential is invalid")]
    InvalidCredential,
}
