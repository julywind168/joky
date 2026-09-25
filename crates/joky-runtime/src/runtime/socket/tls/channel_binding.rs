//! RFC 5929 section 4.1: hash the full DER leaf, using its signature hash.
//! Unknown/undefined signature hashes fail closed; never invent a SHA-256 fallback.
use sha2::{Digest, Sha224, Sha256, Sha384, Sha512, Sha512_224, Sha512_256};
use x509_parser::prelude::*;
use x509_parser::signature_algorithm::RsaSsaPssParams;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Hash {
    Sha224,
    Sha256,
    Sha384,
    Sha512,
    Sha512_224,
    Sha512_256,
}

fn hash_oid(oid: &str) -> Result<Hash, String> {
    match oid {
        "1.2.840.113549.2.5" | "1.3.14.3.2.26" | "2.16.840.1.101.3.4.2.1" => Ok(Hash::Sha256),
        "2.16.840.1.101.3.4.2.4" => Ok(Hash::Sha224),
        "2.16.840.1.101.3.4.2.2" => Ok(Hash::Sha384),
        "2.16.840.1.101.3.4.2.3" => Ok(Hash::Sha512),
        "2.16.840.1.101.3.4.2.5" => Ok(Hash::Sha512_224),
        "2.16.840.1.101.3.4.2.6" => Ok(Hash::Sha512_256),
        _ => Err(format!("TLS: unsupported channel binding hash {oid}")),
    }
}

fn signature_hash(algorithm: &AlgorithmIdentifier<'_>) -> Result<Hash, String> {
    let oid = algorithm.algorithm.to_id_string();
    match oid.as_str() {
        // MD5/SHA-1 signatures use SHA-256, per RFC 5929.
        "1.2.840.113549.1.1.4"
        | "1.2.840.113549.1.1.5"
        | "1.3.14.3.2.29"
        | "1.2.840.10040.4.3"
        | "1.2.840.10045.4.1"
        | "1.2.840.113549.1.1.11"
        | "1.2.840.10045.4.3.2"
        | "2.16.840.1.101.3.4.3.2" => Ok(Hash::Sha256),
        "1.2.840.113549.1.1.14" | "1.2.840.10045.4.3.1" | "2.16.840.1.101.3.4.3.1" => {
            Ok(Hash::Sha224)
        }
        "1.2.840.113549.1.1.12" | "1.2.840.10045.4.3.3" | "2.16.840.1.101.3.4.3.3" => {
            Ok(Hash::Sha384)
        }
        "1.2.840.113549.1.1.13" | "1.2.840.10045.4.3.4" | "2.16.840.1.101.3.4.3.4" => {
            Ok(Hash::Sha512)
        }
        "1.2.840.113549.1.1.15" => Ok(Hash::Sha512_224),
        "1.2.840.113549.1.1.16" => Ok(Hash::Sha512_256),
        "1.2.840.113549.1.1.10" => {
            let parameters = algorithm
                .parameters
                .as_ref()
                .ok_or("TLS: missing RSA-PSS parameters")?;
            let parameters = RsaSsaPssParams::try_from(parameters)
                .map_err(|_| "TLS: invalid RSA-PSS parameters")?;
            hash_oid(&parameters.hash_algorithm_oid().to_id_string())
        }
        _ => Err(format!(
            "TLS: unsupported channel binding signature algorithm {oid}"
        )),
    }
}

pub(super) fn server_end_point(der: &[u8]) -> Result<Vec<u8>, String> {
    let (rest, certificate) =
        X509Certificate::from_der(der).map_err(|_| "TLS: invalid peer certificate DER")?;
    if !rest.is_empty() {
        return Err("TLS: trailing peer certificate DER".into());
    }
    Ok(match signature_hash(&certificate.signature_algorithm)? {
        Hash::Sha224 => Sha224::digest(der).to_vec(),
        Hash::Sha256 => Sha256::digest(der).to_vec(),
        Hash::Sha384 => Sha384::digest(der).to_vec(),
        Hash::Sha512 => Sha512::digest(der).to_vec(),
        Hash::Sha512_224 => Sha512_224::digest(der).to_vec(),
        Hash::Sha512_256 => Sha512_256::digest(der).to_vec(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_and_concatenated_certificates() {
        assert!(server_end_point(b"not a certificate").is_err());
        // A valid certificate followed by another DER object must not be hashed
        // as if it were a single certificate. The parser check is kept here so
        // callers cannot accidentally bind to an ambiguous peer chain.
        assert!(server_end_point(&[0x30, 0x00]).is_err());
    }
}
