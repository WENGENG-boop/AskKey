use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use hkdf::Hkdf;
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

use crate::VaultError;

type HmacSha256 = Hmac<Sha256>;

pub(crate) const KEY_BYTES: usize = 32;
const NONCE_BYTES: usize = 12;

/// Keys derived from the App-owned vault key.
pub(crate) struct VaultKeys {
    cipher: Aes256Gcm,
    authentication: Zeroizing<[u8; 32]>,
    name_index: Zeroizing<[u8; 32]>,
}

fn derive(vault_key: &[u8], info: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut output = Zeroizing::new([0u8; 32]);
    Hkdf::<Sha256>::new(Some(b"askkey-windows-v1"), vault_key)
        .expand(info, output.as_mut())
        .expect("32 bytes is a valid HKDF length");
    output
}

impl VaultKeys {
    pub fn new(vault_key: &[u8]) -> Result<Self, VaultError> {
        if vault_key.len() != KEY_BYTES {
            return Err(VaultError::Authentication);
        }
        let mut encryption = derive(vault_key, b"field-encryption");
        let cipher = Aes256Gcm::new_from_slice(encryption.as_ref()).expect("32-byte key");
        encryption.zeroize();
        Ok(Self {
            cipher,
            authentication: derive(vault_key, b"record-authentication"),
            name_index: derive(vault_key, b"name-index"),
        })
    }

    pub fn generate_key() -> Zeroizing<Vec<u8>> {
        let mut key = Zeroizing::new(vec![0u8; KEY_BYTES]);
        rand::rngs::OsRng.fill_bytes(&mut key);
        key
    }

    /// AES-256-GCM with a random nonce; `context` is authenticated as AAD and
    /// ties the ciphertext to its record and field.
    pub fn seal(&self, plaintext: &[u8], context: &str) -> Vec<u8> {
        let mut nonce = [0u8; NONCE_BYTES];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let ciphertext = self
            .cipher
            .encrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: plaintext,
                    aad: context.as_bytes(),
                },
            )
            .expect("AES-GCM encryption does not fail for bounded input");
        [nonce.as_slice(), &ciphertext].concat()
    }

    pub fn open(&self, sealed: &[u8], context: &str) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        if sealed.len() < NONCE_BYTES {
            return Err(VaultError::Authentication);
        }
        let (nonce, ciphertext) = sealed.split_at(NONCE_BYTES);
        self.cipher
            .decrypt(
                Nonce::from_slice(nonce),
                Payload {
                    msg: ciphertext,
                    aad: context.as_bytes(),
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| VaultError::Authentication)
    }

    fn mac(&self, fields: &[&[u8]]) -> HmacSha256 {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(self.authentication.as_ref())
            .expect("any key length");
        for field in fields {
            mac.update(&(field.len() as u64).to_be_bytes());
            mac.update(field);
        }
        mac
    }

    /// Authenticates a whole record, including its plaintext policy columns.
    pub fn record_tag(&self, fields: &[&[u8]]) -> Vec<u8> {
        self.mac(fields).finalize().into_bytes().to_vec()
    }

    pub fn verify_record(&self, fields: &[&[u8]], tag: &[u8]) -> Result<(), VaultError> {
        self.mac(fields)
            .verify_slice(tag)
            .map_err(|_| VaultError::Authentication)
    }

    /// Keyed index of a normalized name, so names stay encrypted but unique.
    pub fn name_key(&self, normalized: &str) -> Vec<u8> {
        let mut mac =
            <HmacSha256 as Mac>::new_from_slice(self.name_index.as_ref()).expect("any key length");
        mac.update(normalized.as_bytes());
        mac.finalize().into_bytes().to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_binds_context() {
        let keys = VaultKeys::new(&VaultKeys::generate_key()).unwrap();
        let sealed = keys.seal(b"synthetic", "askkey/id-1/secret");
        assert_eq!(
            keys.open(&sealed, "askkey/id-1/secret").unwrap().as_slice(),
            b"synthetic"
        );
        assert!(keys.open(&sealed, "askkey/id-2/secret").is_err());
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(keys.open(&tampered, "askkey/id-1/secret").is_err());
    }

    #[test]
    fn record_tags_detect_policy_changes() {
        let keys = VaultKeys::new(&VaultKeys::generate_key()).unwrap();
        let tag = keys.record_tag(&[b"id", b"ask"]);
        assert!(keys.verify_record(&[b"id", b"ask"], &tag).is_ok());
        assert!(keys.verify_record(&[b"id", b"allow"], &tag).is_err());
        // Length prefixes keep field boundaries distinct.
        assert!(keys.verify_record(&[b"ida", b"sk"], &tag).is_err());
    }
}
