use aes_gcm::{Aes256Gcm, KeyInit, Nonce, aead::Aead};
use hmac::{Hmac, Mac};
use rand::{RngCore, rngs::OsRng};
use sha2::Sha256;

use crate::{error::ApiError, parser::Proxy};

#[derive(Clone)]
pub(crate) struct SecretStore {
    cipher: Aes256Gcm,
    fingerprint_key: [u8; 32],
}

impl SecretStore {
    pub fn new(key: &[u8; 32]) -> Self {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("valid HMAC key size");
        mac.update(b"proxy-microservice/fingerprint-key/v1");
        Self {
            cipher: Aes256Gcm::new(key.into()),
            fingerprint_key: mac.finalize().into_bytes().into(),
        }
    }

    pub fn encrypt(&self, password: &str) -> Result<Vec<u8>, ApiError> {
        let mut nonce = [0; 12];
        OsRng.fill_bytes(&mut nonce);
        let ciphertext = self
            .cipher
            .encrypt(Nonce::from_slice(&nonce), password.as_bytes())
            .map_err(|_| ApiError::internal())?;
        Ok([nonce.as_slice(), &ciphertext].concat())
    }

    pub fn decrypt(&self, encrypted: &[u8]) -> Result<String, ApiError> {
        if encrypted.len() < 28 {
            return Err(ApiError::internal());
        }
        let plaintext = self
            .cipher
            .decrypt(Nonce::from_slice(&encrypted[..12]), &encrypted[12..])
            .map_err(|_| ApiError::internal())?;
        String::from_utf8(plaintext).map_err(|_| ApiError::internal())
    }

    pub fn fingerprint(&self, proxy: &Proxy) -> Vec<u8> {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.fingerprint_key)
            .expect("valid HMAC key size");
        // Length prefixes prevent field-boundary collisions, even with unusual passwords.
        for field in [
            proxy.protocol.as_str(),
            &proxy.host,
            &proxy.port.to_string(),
            &proxy.username,
            &proxy.password,
        ] {
            mac.update(&(field.len() as u64).to_be_bytes());
            mac.update(field.as_bytes());
        }
        mac.finalize().into_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encryption_is_randomized_and_authenticated() {
        let store = SecretStore::new(&[7; 32]);
        let first = store.encrypt("synthetic-password").ok().unwrap();
        let mut second = store.encrypt("synthetic-password").ok().unwrap();
        assert_ne!(first, second);
        assert_eq!(store.decrypt(&first).ok().unwrap(), "synthetic-password");
        second[15] ^= 1;
        assert!(store.decrypt(&second).is_err());
        assert!(SecretStore::new(&[8; 32]).decrypt(&first).is_err());
    }
}
