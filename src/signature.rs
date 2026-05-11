use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

pub fn verify_github_signature(secret: &str, signature_header: Option<&str>, body: &[u8]) -> bool {
    let Some(signature_header) = signature_header else {
        return false;
    };

    let Some(hex_signature) = signature_header.strip_prefix("sha256=") else {
        return false;
    };

    let Ok(signature) = hex::decode(hex_signature) else {
        return false;
    };

    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };

    mac.update(body);
    mac.verify_slice(&signature).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_valid_signature() {
        let body = br#"{"ok":true}"#;
        let mut mac = HmacSha256::new_from_slice(b"secret").unwrap();
        mac.update(body);
        let signature = format!("sha256={}", hex::encode(mac.finalize().into_bytes()));

        assert!(verify_github_signature("secret", Some(&signature), body));
        assert!(!verify_github_signature("wrong", Some(&signature), body));
    }
}
