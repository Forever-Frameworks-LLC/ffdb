use ffdb_protocol::{ApiKeyId, DeveloperPrincipal, DeveloperScope, OrganizationId, ProjectId};

use crate::{
    CredentialDigest, OpaqueTokenCodec, SecretToken, TokenParts, opaque_token::TokenError,
};

#[derive(Clone, Debug)]
pub struct ApiKeyCodec(OpaqueTokenCodec);

impl ApiKeyCodec {
    pub fn new(pepper: Vec<u8>) -> Result<Self, TokenError> {
        OpaqueTokenCodec::new("dev", pepper).map(Self)
    }

    pub fn issue(
        &self,
        organization_id: OrganizationId,
        project_id: Option<ProjectId>,
        scopes: Vec<DeveloperScope>,
    ) -> Result<ApiKeyIssue, TokenError> {
        let (plaintext, parts) = self.0.issue()?;
        Ok(ApiKeyIssue {
            plaintext,
            record: ApiKeyRecord {
                id: ApiKeyId::new(),
                organization_id,
                project_id,
                prefix: parts.prefix,
                digest: parts.digest,
                scopes,
                expires_at_ms: None,
                revoked_at_ms: None,
            },
        })
    }

    pub fn candidate(&self, token: &str) -> Result<TokenParts, TokenError> {
        self.0.parse_and_digest(token)
    }

    #[must_use]
    pub fn verify(
        &self,
        candidate: &TokenParts,
        record: &ApiKeyRecord,
        now_ms: i64,
    ) -> ApiKeyVerification {
        if candidate.prefix != record.prefix
            || !self.0.verify_digest(&candidate.digest, &record.digest)
            || record.revoked_at_ms.is_some()
            || record.expires_at_ms.is_some_and(|expiry| now_ms >= expiry)
        {
            return ApiKeyVerification::Rejected;
        }
        ApiKeyVerification::Verified(DeveloperPrincipal {
            organization_id: record.organization_id,
            api_key_id: record.id,
            scopes: record.scopes.clone(),
            actor_label: format!("api-key:{}", record.id),
        })
    }
}

#[derive(Debug)]
pub struct ApiKeyIssue {
    pub plaintext: SecretToken,
    pub record: ApiKeyRecord,
}

#[derive(Clone, Debug)]
pub struct ApiKeyRecord {
    pub id: ApiKeyId,
    pub organization_id: OrganizationId,
    pub project_id: Option<ProjectId>,
    pub prefix: String,
    pub digest: CredentialDigest,
    pub scopes: Vec<DeveloperScope>,
    pub expires_at_ms: Option<i64>,
    pub revoked_at_ms: Option<i64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ApiKeyVerification {
    Verified(DeveloperPrincipal),
    Rejected,
}

#[must_use]
pub fn api_key_has_scope(record: &ApiKeyRecord, required: DeveloperScope) -> bool {
    record.scopes.contains(&required)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_scope_and_rejects_revocation() -> Result<(), TokenError> {
        let codec = ApiKeyCodec::new(vec![42; 32])?;
        let issue = codec.issue(
            OrganizationId::new(),
            Some(ProjectId::new()),
            vec![DeveloperScope::DatabaseQuery],
        )?;
        let candidate = codec.candidate(issue.plaintext.expose())?;
        assert!(matches!(
            codec.verify(&candidate, &issue.record, 10),
            ApiKeyVerification::Verified(_)
        ));
        assert!(api_key_has_scope(
            &issue.record,
            DeveloperScope::DatabaseQuery
        ));

        let mut revoked = issue.record;
        revoked.revoked_at_ms = Some(9);
        assert_eq!(
            codec.verify(&candidate, &revoked, 10),
            ApiKeyVerification::Rejected
        );
        Ok(())
    }

    #[test]
    fn wrong_pepper_never_verifies() -> Result<(), TokenError> {
        let issuer = ApiKeyCodec::new(vec![1; 32])?;
        let verifier = ApiKeyCodec::new(vec![2; 32])?;
        let issue = issuer.issue(
            OrganizationId::new(),
            None,
            vec![DeveloperScope::ProjectsRead],
        )?;
        let candidate = verifier.candidate(issue.plaintext.expose())?;
        assert_eq!(
            verifier.verify(&candidate, &issue.record, 0),
            ApiKeyVerification::Rejected
        );
        Ok(())
    }
}
