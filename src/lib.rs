use anyhow::{anyhow, Result};
use jsonwebtoken::{decode, Algorithm, DecodingKey, Validation};
use log::{debug, error, info, warn};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::RwLock;

/// Represents a JSON Web Key (JWK) used for token validation.
///
/// A JWK is a digital secure key used in secure web communications.
/// It contains all the important details about the key, such as what it's for
/// and how it works. This information helps websites verify users.
#[derive(Debug, Serialize, Deserialize)]
pub struct JWK {
    /// Key type (e.g., "RSA")
    pub kty: String,
    /// Intended use of the key (e.g., "sig" for signature)
    pub use_: Option<String>,
    /// Unique identifier for the key
    pub kid: String,
    /// Algorithm used with this key (e.g., "RS256")
    pub alg: Option<String>,
    /// RSA public key modulus (base64url-encoded)
    pub n: String,
    /// RSA public key exponent (base64url-encoded)
    pub e: String,
    /// X.509 certificate chain (optional)
    pub x5c: Option<Vec<String>>,
    /// X.509 certificate SHA-1 thumbprint (optional)
    pub x5t: Option<String>,
    /// X.509 certificate SHA-256 thumbprint (optional)
    pub x5t_s256: Option<String>,
}

/// Represents a set of JSON Web Keys (JWKS) used for GitHub token validation.
///
/// This structure is crucial for GitHub Actions authentication because:
///
/// 1. GitHub Key Rotation: GitHub rotates its keys for security,
///    and having multiple keys allows your application to validate
///    tokens continuously during these changes.
///
/// 2. Multiple Environments: Different GitHub environments (like production and development)
///    might use different keys. A set of keys allows your app to work across these environments.
///
/// 3. Fallback Mechanism: If one key fails for any reason, your app can try others in the set.
///
/// Think of it like a key ring for a building manager. They don't just carry one key,
/// but a set of keys for different doors or areas.
#[derive(Debug, Serialize, Deserialize)]
pub struct GithubJWKS {
    /// Vector of JSON Web Keys
    pub keys: Vec<JWK>,
}

/// Represents the claims contained in a GitHub Actions JWT (JSON Web Token).
///
/// When a GitHub Actions workflow runs, it receives a token with these claims.
/// This struct helps decode and access the information from that token.
#[derive(Debug, Serialize, Deserialize)]
pub struct GitHubClaims {
    /// The subject of the token (e.g the GitHub Actions runner ID).
    pub subject: String,

    /// The full name of the repository.
    pub repository: String,

    /// The owner of the repository.
    pub repository_owner: String,

    /// A reference to the specific job and workflow.
    pub job_workflow_ref: String,

    /// The timestamp when the token was issued.
    pub iat: u64,
}

/// Fetches the JSON Web Key Set (JWKS) from the specified OIDC URL.
///
/// This function is used to retrieve the set of public keys that GitHub uses
/// to sign its JSON Web Tokens (JWTs).
///
/// # Arguments
///
/// * `oidc_url` - The base URL of the OpenID Connect provider (GitHub in this case)
///
/// # Returns
///
/// * `Result<GithubJWKS>` - A Result containing the fetched JWKS if successful,
///   or an error if the fetch or parsing fails
///
/// # Example
///
/// ```
/// let jwks = fetch_jwks(your_oidc_url).await?;
/// ```
pub async fn fetch_jwks(oidc_url: &str) -> Result<GithubJWKS> {
    info!("Fetching JWKS from {}", oidc_url);
    let client = reqwest::Client::new();
    let jwks_url = format!("{}/.well-known/jwks", oidc_url);
    match client.get(&jwks_url).send().await {
        Ok(response) => match response.json::<GithubJWKS>().await {
            Ok(jwks) => {
                info!("JWKS fetched successfully");
                Ok(jwks)
            }
            Err(e) => {
                error!("Failed to parse JWKS response: {:?}", e);
                Err(anyhow!("Failed to parse JWKS"))
            }
        },
        Err(e) => {
            error!("Failed to fetch JWKS: {:?}", e);
            Err(anyhow!("Failed to fetch JWKS"))
        }
    }
}

impl GithubJWKS {
    pub async fn validate_github_token(
        token: &str,
        jwks: Arc<RwLock<GithubJWKS>>,
        expected_audience: Option<&str>,
    ) -> Result<GitHubClaims> {
        debug!("Starting token validation");
        if !token.starts_with("eyJ") {
            warn!("Invalid token format received");
            return Err(anyhow!("Invalid token format. Expected a JWT."));
        }

        let jwks = jwks.read().await;
        debug!("JWKS loaded");

        let header = jsonwebtoken::decode_header(token).map_err(|e| {
            anyhow!(
                "Failed to decode header: {}. Make sure you're using a valid JWT, not a PAT.",
                e
            )
        })?;

        let decoding_key = if let Some(kid) = header.kid {
            let key = jwks
                .keys
                .iter()
                .find(|k| k.kid == kid)
                .ok_or_else(|| anyhow!("Matching key not found in JWKS"))?;

            let modulus = key.n.as_str();
            let exponent = key.e.as_str();

            DecodingKey::from_rsa_components(modulus, exponent)
                .map_err(|e| anyhow!("Failed to create decoding key: {}", e))?
        } else {
            DecodingKey::from_secret("your_secret_key".as_ref())
        };

        let mut validation = Validation::new(Algorithm::RS256);
        if let Some(audience) = expected_audience {
            validation.set_audience(&[audience]);
        }

        let token_data = decode::<GitHubClaims>(token, &decoding_key, &validation)
            .map_err(|e| anyhow!("Failed to decode token: {}", e))?;

        let claims = token_data.claims;

        if let Ok(org) = std::env::var("GITHUB_ORG") {
            if claims.repository_owner != org {
                warn!(
                    "Token organization mismatch. Expected: {}, Found: {}",
                    org, claims.repository_owner
                );
                return Err(anyhow!("Token is not from the expected organization"));
            }
        }

        if let Ok(repo) = std::env::var("GITHUB_REPO") {
            debug!(
                "Comparing repositories - Expected: {}, Found: {}",
                repo, claims.repository
            );
            if claims.repository != repo {
                warn!(
                    "Token repository mismatch. Expected: {}, Found: {}",
                    repo, claims.repository
                );
                return Err(anyhow!("Token is not from the expected repository"));
            }
        }

        debug!("Token validation completed successfully");
        Ok(claims)
    }
}

pub async fn validate_github_token(
    token: &str,
    jwks: Arc<RwLock<GithubJWKS>>,
    expected_audience: Option<&str>,
) -> Result<GitHubClaims> {
    GithubJWKS::validate_github_token(token, jwks, expected_audience).await
}
