use super::AccountError;
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use ring::{
    pbkdf2,
    rand::{SecureRandom, SystemRandom},
};
use std::num::NonZeroU32;
const ITERATIONS: NonZeroU32 = match NonZeroU32::new(600_000) {
    Some(value) => value,
    None => NonZeroU32::MIN,
};

pub(super) fn validate(password: &str) -> Result<(), AccountError> {
    if !(12..=128).contains(&password.chars().count()) || password.len() > 512 {
        return Err(AccountError::Invalid(
            "A senha deve ter de 12 a 128 caracteres.",
        ));
    }
    Ok(())
}
pub(super) fn hash(password: &str) -> Result<String, AccountError> {
    let mut salt = [0; 32];
    SystemRandom::new()
        .fill(&mut salt)
        .map_err(|_| AccountError::Unavailable)?;
    let mut derived = [0; 32];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        ITERATIONS,
        &salt,
        password.as_bytes(),
        &mut derived,
    );
    Ok(format!(
        "pbkdf2-sha256:600000:{}:{}",
        STANDARD_NO_PAD.encode(salt),
        STANDARD_NO_PAD.encode(derived)
    ))
}
pub(super) fn verify(password: &str, hash: &str) -> bool {
    let pieces: Vec<_> = hash.split(':').collect();
    if pieces.len() != 4 || pieces[0] != "pbkdf2-sha256" || pieces[1] != "600000" {
        return false;
    }
    let (Ok(salt), Ok(expected)) = (
        STANDARD_NO_PAD.decode(pieces[2]),
        STANDARD_NO_PAD.decode(pieces[3]),
    ) else {
        return false;
    };
    if salt.len() != 32 || expected.len() != 32 {
        return false;
    }
    pbkdf2::verify(
        pbkdf2::PBKDF2_HMAC_SHA256,
        ITERATIONS,
        &salt,
        password.as_bytes(),
        &expected,
    )
    .is_ok()
}
pub(super) fn dummy_verify(password: &str) {
    let _ = pbkdf2::verify(
        pbkdf2::PBKDF2_HMAC_SHA256,
        ITERATIONS,
        &[0x78; 32],
        password.as_bytes(),
        &[0x23; 32],
    );
}
pub(super) fn random_token() -> Result<String, AccountError> {
    let mut bytes = [0; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| AccountError::Unavailable)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
