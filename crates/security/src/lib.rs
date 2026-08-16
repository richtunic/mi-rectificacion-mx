use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, KeyInit, Payload},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rand::random;
use thiserror::Error;
use zeroize::Zeroizing;

const FILE_MAGIC: &[u8; 8] = b"MRMXEVD1";
const NONCE_SIZE: usize = 12;
const KEY_SIZE: usize = 32;
const KEYRING_SERVICE: &str = "mx.mirectificacion.app";
const KEYRING_ACCOUNT: &str = "attachment-master-key-v1";

pub struct AttachmentCipher {
    key: Zeroizing<[u8; KEY_SIZE]>,
}

impl std::fmt::Debug for AttachmentCipher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AttachmentCipher")
            .field("key", &"[REDACTED]")
            .finish()
    }
}

impl Clone for AttachmentCipher {
    fn clone(&self) -> Self {
        Self::from_key(*self.key)
    }
}

impl AttachmentCipher {
    pub fn from_key(key: [u8; KEY_SIZE]) -> Self {
        Self {
            key: Zeroizing::new(key),
        }
    }

    pub fn load_or_create_platform() -> Result<Self, SecurityError> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)?;
        match entry.get_password() {
            Ok(encoded) => {
                let decoded = STANDARD
                    .decode(encoded)
                    .map_err(|_| SecurityError::InvalidStoredKey)?;
                let key: [u8; KEY_SIZE] = decoded
                    .try_into()
                    .map_err(|_| SecurityError::InvalidStoredKey)?;
                Ok(Self::from_key(key))
            }
            Err(keyring::Error::NoEntry) => {
                let key = random::<[u8; KEY_SIZE]>();
                entry.set_password(&STANDARD.encode(key))?;
                Ok(Self::from_key(key))
            }
            Err(error) => Err(error.into()),
        }
    }

    pub fn encrypt(
        &self,
        plaintext: &[u8],
        associated_data: &[u8],
    ) -> Result<Vec<u8>, SecurityError> {
        let nonce_bytes = random::<[u8; NONCE_SIZE]>();
        let cipher = Aes256Gcm::new_from_slice(self.key.as_ref())
            .map_err(|_| SecurityError::EncryptionFailed)?;
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&nonce_bytes),
                Payload {
                    msg: plaintext,
                    aad: associated_data,
                },
            )
            .map_err(|_| SecurityError::EncryptionFailed)?;

        let mut output = Vec::with_capacity(FILE_MAGIC.len() + NONCE_SIZE + ciphertext.len());
        output.extend_from_slice(FILE_MAGIC);
        output.extend_from_slice(&nonce_bytes);
        output.extend_from_slice(&ciphertext);
        Ok(output)
    }

    pub fn decrypt(
        &self,
        encrypted: &[u8],
        associated_data: &[u8],
    ) -> Result<Vec<u8>, SecurityError> {
        if encrypted.len() <= FILE_MAGIC.len() + NONCE_SIZE
            || &encrypted[..FILE_MAGIC.len()] != FILE_MAGIC
        {
            return Err(SecurityError::InvalidEncryptedFile);
        }

        let nonce_start = FILE_MAGIC.len();
        let ciphertext_start = nonce_start + NONCE_SIZE;
        let cipher = Aes256Gcm::new_from_slice(self.key.as_ref())
            .map_err(|_| SecurityError::DecryptionFailed)?;
        cipher
            .decrypt(
                Nonce::from_slice(&encrypted[nonce_start..ciphertext_start]),
                Payload {
                    msg: &encrypted[ciphertext_start..],
                    aad: associated_data,
                },
            )
            .map_err(|_| SecurityError::DecryptionFailed)
    }
}

#[derive(Debug, Error)]
pub enum SecurityError {
    #[error("No fue posible acceder al llavero del sistema: {0}")]
    Keyring(#[from] keyring::Error),
    #[error("La llave almacenada en el sistema no es válida")]
    InvalidStoredKey,
    #[error("No fue posible cifrar el documento")]
    EncryptionFailed,
    #[error("No fue posible descifrar el documento")]
    DecryptionFailed,
    #[error("El archivo cifrado no pertenece a Mi Rectificación MX")]
    InvalidEncryptedFile,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypts_and_decrypts_with_associated_document_id() {
        let cipher = AttachmentCipher::from_key([7; KEY_SIZE]);
        let encrypted = cipher.encrypt(b"estado de cuenta", b"document-1").unwrap();

        assert!(
            !encrypted
                .windows(16)
                .any(|part| part == b"estado de cuenta")
        );
        assert_eq!(
            cipher.decrypt(&encrypted, b"document-1").unwrap(),
            b"estado de cuenta"
        );
    }

    #[test]
    fn rejects_tampering_or_a_swapped_document_id() {
        let cipher = AttachmentCipher::from_key([9; KEY_SIZE]);
        let mut encrypted = cipher.encrypt(b"comprobante", b"document-1").unwrap();
        let last = encrypted.len() - 1;
        encrypted[last] ^= 1;

        assert!(cipher.decrypt(&encrypted, b"document-1").is_err());
        let encrypted = cipher.encrypt(b"comprobante", b"document-1").unwrap();
        assert!(cipher.decrypt(&encrypted, b"document-2").is_err());
    }
}
