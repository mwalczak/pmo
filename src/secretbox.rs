//! Encrypts secrets the database stores, such as projects' SMTP passwords, with the MAIL_ENCRYPTION_KEY
//! (XChaCha20-Poly1305). A database dump alone does not reveal them.
use anyhow::{Context, Result, bail};
use chacha20poly1305::{
    Key, XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};

const NONCE: usize = 24;

pub struct SecretBox(XChaCha20Poly1305);

impl SecretBox {
    /// From 32 bytes as 64 hex characters (`openssl rand -hex 32`).
    pub fn from_hex(hex_key: &str) -> Result<Self> {
        let bytes: [u8; 32] = hex::decode(hex_key.trim())
            .context("MAIL_ENCRYPTION_KEY must be hex")?
            .try_into()
            .map_err(|_| anyhow::anyhow!("MAIL_ENCRYPTION_KEY must be 32 bytes (64 hex characters)"))?;
        Ok(Self(XChaCha20Poly1305::new(&Key::from(bytes))))
    }

    /// Nonce and ciphertext. `context` (e.g. the project's id) binds it to its row, so it cannot be moved to another.
    pub fn seal(&self, plaintext: &str, context: &[u8]) -> Vec<u8> {
        let nonce = XNonce::from(rand::random::<[u8; NONCE]>());
        let ciphertext = self
            .0
            .encrypt(&nonce, Payload { msg: plaintext.as_bytes(), aad: context })
            .expect("encrypting in memory cannot fail");
        [nonce.as_slice(), &ciphertext].concat()
    }

    pub fn open(&self, sealed: &[u8], context: &[u8]) -> Result<String> {
        if sealed.len() <= NONCE {
            bail!("sealed secret too short");
        }
        let (nonce, ciphertext) = sealed.split_at(NONCE);
        let nonce = XNonce::try_from(nonce).expect("24 bytes");
        let plaintext = self
            .0
            .decrypt(&nonce, Payload { msg: ciphertext, aad: context })
            .map_err(|_| anyhow::anyhow!("cannot decrypt: wrong MAIL_ENCRYPTION_KEY, or not this row's secret"))?;
        Ok(String::from_utf8(plaintext)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f";

    #[test]
    fn seals_and_opens() {
        let secrets = SecretBox::from_hex(KEY).unwrap();
        let sealed = secrets.seal("hasło", b"project-1");
        assert!(!sealed.windows(5).any(|w| w == "hasło".as_bytes()));
        assert_ne!(sealed, secrets.seal("hasło", b"project-1"), "a fresh nonce every time");
        assert_eq!(secrets.open(&sealed, b"project-1").unwrap(), "hasło");
        assert!(secrets.open(&sealed, b"project-2").is_err(), "bound to its row");
        let other = SecretBox::from_hex(&KEY.replace('0', "f")).unwrap();
        assert!(other.open(&sealed, b"project-1").is_err(), "needs the same key");
        assert!(SecretBox::from_hex("abcd").is_err());
    }
}
