use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::RngCore;
use sha2::{Digest, Sha256};

pub fn random_secret(bytes: usize) -> String {
    let mut raw = vec![0_u8; bytes];
    rand::rng().fill_bytes(&mut raw);
    URL_SAFE_NO_PAD.encode(raw)
}

pub fn digest(secret: &str) -> Vec<u8> {
    Sha256::digest(secret.as_bytes()).to_vec()
}

pub fn verify_digest(secret: &str, expected: &[u8]) -> bool {
    constant_time_eq::constant_time_eq(&digest(secret), expected)
}

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let mut salt_bytes = [0_u8; 16];
    rand::rng().fill_bytes(&mut salt_bytes);
    let salt = SaltString::encode_b64(&salt_bytes)
        .map_err(|error| anyhow::anyhow!("could not encode password salt: {error}"))?;
    Ok(Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map_err(|error| anyhow::anyhow!("could not hash password: {error}"))?
        .to_string())
}

pub fn verify_password(password: &str, encoded: &str) -> bool {
    PasswordHash::new(encoded).ok().is_some_and(|hash| {
        Argon2::default()
            .verify_password(password.as_bytes(), &hash)
            .is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn secrets_are_long_and_verifiable() {
        let value = random_secret(32);
        assert!(value.len() >= 43);
        assert!(verify_digest(&value, &digest(&value)));
        assert!(!verify_digest("different", &digest(&value)));
    }
    #[test]
    fn passwords_use_argon2() {
        let hash = hash_password("correct horse").unwrap();
        assert!(hash.starts_with("$argon2"));
        assert!(verify_password("correct horse", &hash));
        assert!(!verify_password("wrong", &hash));
    }
}
