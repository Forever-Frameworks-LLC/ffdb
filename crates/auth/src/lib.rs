//! Authentication primitives and state machines for FFDB.
//!
//! Bearer credentials are returned once and represented by redacting secret
//! types. Persistent records contain only a lookup prefix and an HMAC-SHA-256
//! digest keyed by deployment secret material.

mod account;
mod api_key;
mod credential;
mod jwt;
mod key_management;
mod one_time;
mod opaque_token;
mod password;
mod postgres;
mod refresh;

pub use api_key::{ApiKeyCodec, ApiKeyIssue, ApiKeyRecord, ApiKeyVerification, api_key_has_scope};
pub use credential::{
    CredentialVerificationError, CredentialVerifierService, PgCredentialVerifier,
};
pub use jwt::{
    AccessTokenClaims, AccessTokenSessionPolicy, ExpectedAccessToken, JwtError, JwtIssuer,
    ProjectSigner, SigningKeyStore, VerificationKey, VerificationKeyStatus, VerifiedAccessToken,
};
pub use key_management::{
    AeadSigningKeyEnvelope, PgSigningKeyManager, SigningKeyDescriptor, SigningKeyEncryptor,
    SigningKeyManagementError, SigningKeyRotation,
};
pub use one_time::{
    InMemoryOneTimeTokenStore, OneTimePurpose, OneTimeStoreError, OneTimeToken, OneTimeTokenRecord,
    OneTimeTokenStore,
};
pub use opaque_token::{CredentialDigest, OpaqueTokenCodec, SecretToken, TokenError, TokenParts};
pub use password::{
    Argon2PasswordHasher, PasswordError, PasswordHash, PasswordHasher, SecretString, VerifyOutcome,
};
pub use postgres::{
    ApiKeyRepository, AuthSessionSummary, EncryptedSigningKey, PgAccountRepository,
    PgApiKeyRepository, PgOneTimeTokenStore, PgRefreshStore, PgSigningKeyStore,
    SigningKeyDecryptor,
};
pub use refresh::{
    InMemoryRefreshStore, RefreshFamily, RefreshIssue, RefreshRotation, RefreshStoreError,
    RefreshTokenRecord, RefreshTokenStore, SessionRecord,
};

/// Maximum accepted bearer-token length. This bounds decode and JSON work.
pub const MAX_BEARER_TOKEN_BYTES: usize = 16 * 1024;
pub use account::{
    AccountError, AccountRepository, AccountService, AuthUserRecord, AuthenticatedUser,
    InMemoryAccountRepository,
};
