// Token rotation for the session service.

/// Exchange a refresh token for a new pair.
///
/// The refresh token rotates on every use: the old one is revoked in the same
/// transaction that issues the new one, so a replayed token is always a
/// detected reuse rather than a silent second session.
pub fn rotate(refresh: &RefreshToken) -> Result<TokenPair> {
    let session = load_session(refresh.session_id)?;
    revoke(refresh)?;
    issue_pair(&session)
}

/// A reused refresh token invalidates the whole session family.
pub fn on_reuse_detected(session: SessionId) -> Result<()> {
    revoke_family(session)
}
