//! Release signing (run on the build machine only).
//!   cargo run -p p2c-update --example release_key -- new  <key-file>
//!   cargo run -p p2c-update --example release_key -- sign <key-file> <file>   (writes <file>.sig)
//! The key file holds the private seed in base64; keep it secret and backed up.
use std::io::Read;

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};

fn main() -> anyhow::Result<()> {
    let b64 = base64::engine::general_purpose::STANDARD;
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["new", key_file] => {
            anyhow::ensure!(
                !std::path::Path::new(key_file).exists(),
                "{key_file} already exists"
            );
            let mut seed = [0u8; 32];
            std::fs::File::open("/dev/urandom")?.read_exact(&mut seed)?;
            let key = SigningKey::from_bytes(&seed);
            write_private(key_file, &b64.encode(seed))?;
            println!("public key: {}", b64.encode(key.verifying_key().to_bytes()));
        }
        ["sign", key_file, file] => {
            let seed: [u8; 32] = b64
                .decode(std::fs::read_to_string(key_file)?.trim())?
                .try_into()
                .map_err(|_| anyhow::anyhow!("bad key file"))?;
            let key = SigningKey::from_bytes(&seed);
            let data = std::fs::read(file)?;
            let sig = b64.encode(key.sign(&data).to_bytes());
            std::fs::write(format!("{file}.sig"), format!("{sig}\n"))?;
            println!(
                "signed {file} with {}",
                b64.encode(key.verifying_key().to_bytes())
            );
        }
        _ => anyhow::bail!("usage: release_key new <key-file> | sign <key-file> <file>"),
    }
    Ok(())
}

#[cfg(unix)]
fn write_private(path: &str, text: &str) -> anyhow::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    writeln!(f, "{text}")?;
    Ok(())
}

#[cfg(not(unix))]
fn write_private(path: &str, text: &str) -> anyhow::Result<()> {
    std::fs::write(path, format!("{text}\n"))?;
    Ok(())
}
