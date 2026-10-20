//! Locked Folder: photos encrypted at rest with a key only the passphrase can unlock.
//!
//! - The passphrase goes through Argon2id (64 MiB, 3 passes) to make a key-encryption key.
//!   That unwraps a random 256-bit data key, so changing the passphrase never re-encrypts photos.
//! - Each photo is stored as two files with random names: `<id>.meta` (name, dates, size,
//!   thumbnail) and `<id>.data` (the original bytes). Both use XChaCha20-Poly1305. Data is
//!   encrypted in 1 MiB chunks (the STREAM construction: per-chunk nonce = random prefix,
//!   chunk counter, last-chunk flag), so reordering, truncating or editing a file is detected.
//!   The item id is bound in as associated data, so files can't be swapped between items.
//! - Nothing decrypted is written to disk; thumbnails and photos are decrypted in memory.
//!
//! Security rests on the passphrase: a long one (four or more random words) is out of reach of
//! guessing, a short one is not. There is no recovery: forget it and the photos are gone.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"BNVAULT1";
const CHUNK: usize = 1 << 20;
const TAG: usize = 16;
const KDF_MEMORY_KIB: u32 = 64 * 1024;
const KDF_PASSES: u32 = 3;
const MIN_PASSPHRASE: usize = 8;

#[derive(Serialize, Deserialize)]
struct Header {
    version: u32,
    kdf_memory_kib: u32,
    kdf_passes: u32,
    kdf_lanes: u32,
    salt: String,
    /// nonce (24) + encrypted data key (32) + tag (16), hex.
    wrapped_key: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Meta {
    pub name: String,
    /// Where the photo was before it was locked; "Move out" puts it back there.
    pub original: PathBuf,
    pub taken: NaiveDateTime,
    pub size: u64,
    pub is_video: bool,
    pub aspect: f32,
    /// Small JPEG, hex.
    thumb: String,
}

impl Meta {
    pub fn new(original: &Path, taken: NaiveDateTime, size: u64, is_video: bool, aspect: f32, thumb_jpeg: &[u8]) -> Self {
        Self {
            name: original.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default(),
            original: original.to_path_buf(),
            taken,
            size,
            is_video,
            aspect,
            thumb: hex(thumb_jpeg),
        }
    }

    pub fn thumb_jpeg(&self) -> Vec<u8> {
        unhex(&self.thumb).unwrap_or_default()
    }
}

#[derive(Clone)]
pub struct Item {
    pub id: String,
    pub meta: Meta,
}

/// An unlocked vault: its folder and the data key. Cheap to clone for background work;
/// the key is wiped from memory when the last clone is dropped.
#[derive(Clone)]
pub struct Vault {
    dir: PathBuf,
    key: Arc<Zeroizing<[u8; 32]>>,
}

#[derive(Debug)]
pub enum Error {
    WrongPassphrase,
    WeakPassphrase,
    Corrupt(String),
    Io(std::io::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::WrongPassphrase => write!(f, "Wrong passphrase."),
            Error::WeakPassphrase => write!(f, "Use at least {MIN_PASSPHRASE} characters; four or more random words is best."),
            Error::Corrupt(what) => write!(f, "Locked Folder data is damaged: {what}"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

pub type Result<T> = std::result::Result<T, Error>;

pub fn dir() -> Option<PathBuf> {
    Some(crate::store::data_dir()?.join("vault"))
}

pub fn exists() -> bool {
    dir().is_some_and(|d| d.join("vault.json").is_file())
}

/// Rough strength hint for the setup screen.
pub fn strength(passphrase: &str) -> &'static str {
    let len = passphrase.chars().count();
    let classes = [
        passphrase.chars().any(|c| c.is_lowercase()),
        passphrase.chars().any(|c| c.is_uppercase()),
        passphrase.chars().any(|c| c.is_ascii_digit()),
        passphrase.chars().any(|c| !c.is_alphanumeric()),
    ]
    .iter()
    .filter(|b| **b)
    .count();
    let words = passphrase.split_whitespace().count();
    match (len, classes, words) {
        (0, _, _) => "",
        (l, _, _) if l < MIN_PASSPHRASE => "Too short",
        (_, _, w) if w >= 4 && len >= 20 => "Very strong",
        (l, c, _) if l >= 16 && c >= 3 => "Very strong",
        (l, c, _) if l >= 12 && c >= 2 => "Strong",
        _ => "Okay. Longer is better",
    }
}

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::getrandom(&mut b).expect("system random number generator");
    b
}

fn kek(passphrase: &str, salt: &[u8], memory: u32, passes: u32, lanes: u32) -> Result<Zeroizing<[u8; 32]>> {
    let params = Params::new(memory, passes, lanes, Some(32)).map_err(|e| Error::Corrupt(e.to_string()))?;
    let mut out = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, out.as_mut())
        .map_err(|e| Error::Corrupt(e.to_string()))?;
    Ok(out)
}

impl Vault {
    /// Sets up a new, empty vault protected by `passphrase`.
    pub fn create(passphrase: &str) -> Result<Self> {
        if passphrase.chars().count() < MIN_PASSPHRASE {
            return Err(Error::WeakPassphrase);
        }
        let dir = dir().ok_or_else(|| Error::Corrupt("no data folder".into()))?;
        std::fs::create_dir_all(&dir)?;
        let salt: [u8; 16] = random();
        let data_key = Zeroizing::new(random::<32>());
        let kek = kek(passphrase, &salt, KDF_MEMORY_KIB, KDF_PASSES, 1)?;
        let nonce: [u8; 24] = random();
        let wrapped = XChaCha20Poly1305::new(kek.as_ref().into())
            .encrypt(XNonce::from_slice(&nonce), Payload { msg: data_key.as_ref(), aad: b"banana-vault-key" })
            .map_err(|_| Error::Corrupt("key wrap".into()))?;
        let header = Header {
            version: 1,
            kdf_memory_kib: KDF_MEMORY_KIB,
            kdf_passes: KDF_PASSES,
            kdf_lanes: 1,
            salt: hex(&salt),
            wrapped_key: hex(&[nonce.as_slice(), &wrapped].concat()),
        };
        write_atomic(&dir.join("vault.json"), &serde_json::to_vec_pretty(&header).unwrap())?;
        Ok(Self { dir, key: Arc::new(data_key) })
    }

    /// Opens the vault, or `WrongPassphrase`.
    pub fn unlock(passphrase: &str) -> Result<Self> {
        let dir = dir().ok_or_else(|| Error::Corrupt("no data folder".into()))?;
        let header: Header = serde_json::from_slice(&std::fs::read(dir.join("vault.json"))?).map_err(|e| Error::Corrupt(e.to_string()))?;
        let salt = unhex(&header.salt).ok_or_else(|| Error::Corrupt("salt".into()))?;
        let wrapped = unhex(&header.wrapped_key).filter(|w| w.len() == 24 + 32 + TAG).ok_or_else(|| Error::Corrupt("key".into()))?;
        let kek = kek(passphrase, &salt, header.kdf_memory_kib, header.kdf_passes, header.kdf_lanes)?;
        let key = XChaCha20Poly1305::new(kek.as_ref().into())
            .decrypt(XNonce::from_slice(&wrapped[..24]), Payload { msg: &wrapped[24..], aad: b"banana-vault-key" })
            .map_err(|_| Error::WrongPassphrase)?;
        let mut data_key = Zeroizing::new([0u8; 32]);
        data_key.copy_from_slice(&key);
        drop(Zeroizing::new(key));
        Ok(Self { dir, key: Arc::new(data_key) })
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new(self.key.as_ref().as_ref().into())
    }

    /// Every item's metadata (skipping, not failing on, damaged ones), newest first.
    pub fn items(&self) -> Vec<Item> {
        let mut items: Vec<Item> = std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                let id = name.strip_suffix(".meta")?.to_string();
                let meta = self.read_meta(&id).ok()?;
                Some(Item { id, meta })
            })
            .collect();
        items.sort_by(|a, b| b.meta.taken.cmp(&a.meta.taken));
        items
    }

    fn read_meta(&self, id: &str) -> Result<Meta> {
        let raw = std::fs::read(self.dir.join(format!("{id}.meta")))?;
        if raw.len() < 8 + 24 + TAG || &raw[..8] != MAGIC {
            return Err(Error::Corrupt(format!("{id}.meta")));
        }
        let aad = format!("meta:{id}");
        let plain = Zeroizing::new(
            self.cipher()
                .decrypt(XNonce::from_slice(&raw[8..32]), Payload { msg: &raw[32..], aad: aad.as_bytes() })
                .map_err(|_| Error::Corrupt(format!("{id}.meta")))?,
        );
        serde_json::from_slice(&plain).map_err(|e| Error::Corrupt(e.to_string()))
    }

    /// Encrypts `src` into the vault, then decrypts it again and compares with `src` byte for
    /// byte. Only when that passes is the item kept; the caller may then delete the original.
    pub fn add(&self, src: &Path, meta: &Meta) -> Result<Item> {
        let id = hex(&random::<16>());
        let data = self.dir.join(format!("{id}.data"));
        let tmp = self.dir.join(format!("{id}.data.part"));
        let result = (|| {
            self.encrypt_file(&id, src, &tmp)?;
            self.verify(&id, &tmp, src)?;
            std::fs::rename(&tmp, &data)?;
            let nonce: [u8; 24] = random();
            let json = Zeroizing::new(serde_json::to_vec(meta).unwrap());
            let aad = format!("meta:{id}");
            let sealed = self
                .cipher()
                .encrypt(XNonce::from_slice(&nonce), Payload { msg: &json, aad: aad.as_bytes() })
                .map_err(|_| Error::Corrupt("encrypt".into()))?;
            write_atomic(&self.dir.join(format!("{id}.meta")), &[MAGIC.as_slice(), &nonce, &sealed].concat())?;
            Ok(Item { id: id.clone(), meta: meta.clone() })
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
            let _ = std::fs::remove_file(&data);
        }
        result
    }

    fn chunk_nonce(prefix: &[u8; 19], index: u32, last: bool) -> [u8; 24] {
        let mut n = [0u8; 24];
        n[..19].copy_from_slice(prefix);
        n[19..23].copy_from_slice(&index.to_be_bytes());
        n[23] = last as u8;
        n
    }

    fn encrypt_file(&self, id: &str, src: &Path, out: &Path) -> Result<()> {
        let cipher = self.cipher();
        let prefix: [u8; 19] = random();
        let mut input = File::open(src)?;
        let mut output = File::create(out)?;
        output.write_all(MAGIC)?;
        output.write_all(&prefix)?;
        let mut buf = Zeroizing::new(vec![0u8; CHUNK]);
        let mut next = Zeroizing::new(vec![0u8; CHUNK]);
        let mut len = read_full(&mut input, &mut buf)?;
        let mut index: u32 = 0;
        loop {
            // Look one chunk ahead so the last one can be flagged.
            let next_len = if len == CHUNK { read_full(&mut input, &mut next)? } else { 0 };
            let last = next_len == 0;
            let nonce = Self::chunk_nonce(&prefix, index, last);
            let sealed = cipher
                .encrypt(XNonce::from_slice(&nonce), Payload { msg: &buf[..len], aad: id.as_bytes() })
                .map_err(|_| Error::Corrupt("encrypt".into()))?;
            output.write_all(&sealed)?;
            if last {
                break;
            }
            std::mem::swap(&mut buf, &mut next);
            len = next_len;
            index = index.checked_add(1).ok_or_else(|| Error::Corrupt("file too large".into()))?;
        }
        output.sync_all()?;
        Ok(())
    }

    /// Decrypts `<id>`'s data chunk by chunk, handing each plaintext chunk to `sink`.
    fn decrypt_chunks(&self, id: &str, path: &Path, mut sink: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
        let cipher = self.cipher();
        let mut input = File::open(path)?;
        let mut head = [0u8; 8 + 19];
        input.read_exact(&mut head).map_err(|_| Error::Corrupt(format!("{id}.data")))?;
        if &head[..8] != MAGIC {
            return Err(Error::Corrupt(format!("{id}.data")));
        }
        let prefix: [u8; 19] = head[8..].try_into().unwrap();
        let mut buf = vec![0u8; CHUNK + TAG];
        let mut next = vec![0u8; CHUNK + TAG];
        let mut len = read_full(&mut input, &mut buf)?;
        let mut index: u32 = 0;
        loop {
            let next_len = if len == CHUNK + TAG { read_full(&mut input, &mut next)? } else { 0 };
            let last = next_len == 0;
            let nonce = Self::chunk_nonce(&prefix, index, last);
            let plain = Zeroizing::new(
                cipher
                    .decrypt(XNonce::from_slice(&nonce), Payload { msg: &buf[..len], aad: id.as_bytes() })
                    .map_err(|_| Error::Corrupt(format!("{id}.data chunk {index}")))?,
            );
            sink(&plain)?;
            if last {
                return Ok(());
            }
            std::mem::swap(&mut buf, &mut next);
            len = next_len;
            index += 1;
        }
    }

    fn verify(&self, id: &str, encrypted: &Path, original: &Path) -> Result<()> {
        let mut src = File::open(original)?;
        let mut expect = vec![0u8; CHUNK];
        self.decrypt_chunks(id, encrypted, |plain| {
            let n = read_full(&mut src, &mut expect[..plain.len()])?;
            if n != plain.len() || expect[..n] != *plain {
                return Err(Error::Corrupt("verification failed".into()));
            }
            Ok(())
        })?;
        if read_full(&mut src, &mut expect[..1])? != 0 {
            return Err(Error::Corrupt("verification failed".into()));
        }
        Ok(())
    }

    /// The whole photo, decrypted in memory (for the viewer).
    pub fn read(&self, id: &str) -> Result<Zeroizing<Vec<u8>>> {
        let mut out = Zeroizing::new(Vec::new());
        self.decrypt_chunks(id, &self.dir.join(format!("{id}.data")), |p| {
            out.extend_from_slice(p);
            Ok(())
        })?;
        Ok(out)
    }

    /// Decrypts the item back to where it came from (or a free name next to it, or into
    /// `fallback_dir` if that folder is gone) and removes it from the vault.
    pub fn restore(&self, item: &Item, fallback_dir: &Path) -> Result<PathBuf> {
        let parent = item.meta.original.parent().filter(|p| p.is_dir()).unwrap_or(fallback_dir);
        let dest = free_name(parent, &item.meta.name);
        let tmp = dest.with_extension("banana-part");
        let result = (|| {
            let mut out = File::create(&tmp)?;
            self.decrypt_chunks(&item.id, &self.dir.join(format!("{}.data", item.id)), |p| Ok(out.write_all(p)?))?;
            out.sync_all()?;
            std::fs::rename(&tmp, &dest)?;
            Ok(dest.clone())
        })();
        if result.is_err() {
            let _ = std::fs::remove_file(&tmp);
            return result;
        }
        self.delete(&item.id)?;
        result
    }

    /// Removes an item for good.
    pub fn delete(&self, id: &str) -> Result<()> {
        let _ = std::fs::remove_file(self.dir.join(format!("{id}.data")));
        std::fs::remove_file(self.dir.join(format!("{id}.meta")))?;
        Ok(())
    }
}

fn read_full(r: &mut impl Read, buf: &mut [u8]) -> Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    let mut f = File::create(&tmp)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// `name`, or `name (2)`, `name (3)`… if taken.
fn free_name(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let p = Path::new(name);
    let stem = p.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    let ext = p.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    (2..).map(|i| dir.join(format!("{stem} ({i}){ext}"))).find(|c| !c.exists()).unwrap()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if s.len() % 2 != 0 {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_data() -> PathBuf {
        let d = std::env::temp_dir().join(format!("banana-vault-test-{}", hex(&random::<8>())));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    // One test so BANANA_DATA isn't raced by parallel tests.
    #[test]
    fn round_trip_wrong_passphrase_and_tamper() {
        let data = temp_data();
        std::env::set_var("BANANA_DATA", &data);
        assert!(matches!(Vault::create("short"), Err(Error::WeakPassphrase)));
        let v = Vault::create("correct horse battery staple").unwrap();
        assert!(matches!(Vault::unlock("wrong horse battery staple"), Err(Error::WrongPassphrase)));
        let v2 = Vault::unlock("correct horse battery staple").unwrap();

        // Sizes around the chunk boundary, including empty.
        for size in [0usize, 1, CHUNK - 1, CHUNK, CHUNK + 1, 3 * CHUNK + 7] {
            let src = data.join(format!("photo_{size}.jpg"));
            let bytes: Vec<u8> = (0..size).map(|i| (i * 31 % 251) as u8).collect();
            std::fs::write(&src, &bytes).unwrap();
            let meta = Meta::new(&src, chrono::NaiveDateTime::default(), size as u64, false, 1.0, b"thumb");
            let item = v.add(&src, &meta).unwrap();
            assert_eq!(*v2.read(&item.id).unwrap(), bytes, "size {size}");
            assert_eq!(v2.items().iter().find(|i| i.id == item.id).unwrap().meta.thumb_jpeg(), b"thumb");

            // The ciphertext must not contain the plaintext.
            let enc = std::fs::read(v.dir.join(format!("{}.data", item.id))).unwrap();
            if size > 64 {
                assert!(!enc.windows(64).any(|w| w == &bytes[..64]));
            }

            // Flipping any byte, or cutting the end off, is caught.
            if size > 0 {
                let mut bad = enc.clone();
                let at = 8 + 19 + size / 2;
                bad[at] ^= 1;
                std::fs::write(v.dir.join(format!("{}.data", item.id)), &bad).unwrap();
                assert!(v.read(&item.id).is_err());
                if size > CHUNK {
                    std::fs::write(v.dir.join(format!("{}.data", item.id)), &enc[..8 + 19 + CHUNK + TAG]).unwrap();
                    assert!(v.read(&item.id).is_err(), "truncation after a full chunk must fail");
                }
                std::fs::write(v.dir.join(format!("{}.data", item.id)), &enc).unwrap();
            }

            std::fs::remove_file(&src).unwrap();
            let back = v.restore(&item, &data).unwrap();
            assert_eq!(std::fs::read(&back).unwrap(), bytes);
            assert!(!v.dir.join(format!("{}.meta", item.id)).exists());
        }
        let _ = std::fs::remove_dir_all(&data);
    }
}
