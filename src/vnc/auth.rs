//! The two ways a VNC server can ask for a password: VNC authentication, which encrypts the
//! server's challenge with the password as a DES key, and Apple Remote Desktop authentication,
//! which encrypts a user name and password with a key agreed by Diffie-Hellman.
use aes::Aes128;
use cipher::{BlockCipherEncrypt, KeyInit};
use des::Des;
use md5::{Digest, Md5};
use num_bigint::BigUint;

/// The response to VNC authentication's 16-byte `challenge`. Only the password's first eight
/// bytes count, each with its bits reversed, as the original VNC implementation did.
pub fn vnc_response(password: &str, challenge: &[u8; 16]) -> [u8; 16] {
    let mut key = [0u8; 8];
    for (k, b) in key.iter_mut().zip(password.bytes()) {
        *k = b.reverse_bits();
    }
    let des = Des::new(&key.into());
    let mut response = *challenge;
    for block in response.as_chunks_mut::<8>().0 {
        des.encrypt_block(block.into());
    }
    response
}

/// What the server sends to start Apple Remote Desktop authentication.
pub struct AppleChallenge {
    pub generator: u16,
    pub prime: Vec<u8>,
    pub server_key: Vec<u8>,
}

/// The longest user name or password Apple authentication carries, in bytes, leaving room for
/// the terminating zero in its 64-byte field.
pub const APPLE_FIELD: usize = 63;

/// The longest prime accepted, in bytes. Macs send 128; the work grows with the cube of the
/// length and cannot be interrupted, so a server cannot make Relay compute for long.
pub const MAX_PRIME: usize = 512;

/// The reply to Apple authentication: the encrypted credentials, then this side's public key.
/// `random` fills the private key and the padding after each field.
pub fn apple_response(
    challenge: &AppleChallenge,
    username: &str,
    password: &str,
    random: impl Fn(&mut [u8]) -> Result<(), String>,
) -> Result<Vec<u8>, String> {
    if username.len() > APPLE_FIELD || password.len() > APPLE_FIELD {
        return Err(format!(
            "The user name and password can each be at most {APPLE_FIELD} bytes long."
        ));
    }
    let length = challenge.prime.len();
    if length > MAX_PRIME {
        return Err("The server asked for an unusually long sign-in key.".into());
    }
    let prime = BigUint::from_bytes_be(&challenge.prime);
    if length == 0 || prime <= BigUint::from(2u8) {
        return Err("The server sent invalid sign-in parameters.".into());
    }
    let mut private = vec![0u8; length];
    random(&mut private)?;
    let private = BigUint::from_bytes_be(&private);
    let public = BigUint::from(challenge.generator).modpow(&private, &prime);
    let shared = BigUint::from_bytes_be(&challenge.server_key).modpow(&private, &prime);
    let key: [u8; 16] = Md5::digest(padded(&shared, length)).into();

    // Each field ends with a zero; random bytes fill the rest so nothing about the lengths shows.
    let mut credentials = [0u8; 128];
    random(&mut credentials)?;
    for (field, text) in (credentials.as_chunks_mut::<64>().0.iter_mut()).zip([username, password])
    {
        field[..text.len()].copy_from_slice(text.as_bytes());
        field[text.len()] = 0;
    }
    let aes = Aes128::new(&key.into());
    for block in credentials.as_chunks_mut::<16>().0 {
        aes.encrypt_block(block.into());
    }
    let mut reply = credentials.to_vec();
    reply.extend(padded(&public, length));
    Ok(reply)
}

/// `value` as `length` big-endian bytes.
fn padded(value: &BigUint, length: usize) -> Vec<u8> {
    let bytes = value.to_bytes_be();
    let mut out = vec![0u8; length.saturating_sub(bytes.len())];
    out.extend_from_slice(&bytes[bytes.len().saturating_sub(length)..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use aes::cipher::BlockCipherDecrypt;

    #[test]
    fn vnc_responses_match_the_reference_implementation() {
        // Computed with OpenSSL's DES-ECB and the bit-reversed key, as libvncserver does.
        let challenge = *b"0123456789abcdef";
        assert_eq!(
            vnc_response("password", &challenge),
            [
                0x56, 0x45, 0xab, 0xeb, 0x5f, 0x1e, 0x64, 0x75, 0xe8, 0xfe, 0xb1, 0x1b, 0xeb, 0x66,
                0xea, 0x19
            ]
        );
        // Only the first eight characters count, and an empty password still answers.
        assert_eq!(
            vnc_response("password and more", &challenge),
            vnc_response("password", &challenge)
        );
        assert_eq!(
            vnc_response("", &challenge),
            [
                0xa0, 0x68, 0xdb, 0xea, 0xb7, 0x3d, 0x14, 0x0b, 0x56, 0x54, 0x1e, 0xb3, 0xc7, 0x62,
                0x51, 0x77
            ]
        );
    }

    #[test]
    fn apple_credentials_decrypt_with_the_servers_key() {
        // A small prime keeps the test quick; servers use 128 bytes or more.
        let prime = BigUint::from(0xFFFF_FFFB_u32); // 4294967291, prime
        let generator = 2u16;
        let server_private = BigUint::from(123_456_789_u32);
        let server_key = BigUint::from(generator).modpow(&server_private, &prime);
        let challenge = AppleChallenge {
            generator,
            prime: padded(&prime, 4),
            server_key: padded(&server_key, 4),
        };
        let counter = std::cell::Cell::new(0u8);
        let random = |buf: &mut [u8]| {
            for b in buf {
                counter.set(counter.get().wrapping_add(37));
                *b = counter.get();
            }
            Ok(())
        };
        let reply = apple_response(&challenge, "alex", "s3cret", random).unwrap();
        assert_eq!(reply.len(), 128 + 4);
        // The server derives the same key from the client's public key.
        let client_key = BigUint::from_bytes_be(&reply[128..]);
        let shared = client_key.modpow(&server_private, &prime);
        let key: [u8; 16] = Md5::digest(padded(&shared, 4)).into();
        let aes = Aes128::new(&key.into());
        let mut plain = reply[..128].to_vec();
        for block in plain.as_chunks_mut::<16>().0 {
            aes.decrypt_block(block.into());
        }
        assert_eq!(&plain[..5], b"alex\0");
        assert_eq!(&plain[64..71], b"s3cret\0");
    }

    #[test]
    fn overlong_apple_credentials_are_refused() {
        let challenge = AppleChallenge {
            generator: 2,
            prime: vec![0xFF; 16],
            server_key: vec![0x12; 16],
        };
        let long = "x".repeat(64);
        let none = |_: &mut [u8]| Ok(());
        assert!(apple_response(&challenge, &long, "pw", none).is_err());
        assert!(apple_response(&challenge, "alex", &long, none).is_err());
        let bad = AppleChallenge {
            prime: vec![],
            ..challenge
        };
        assert!(apple_response(&bad, "alex", "pw", none).is_err());
        let huge = AppleChallenge {
            prime: vec![0xFF; MAX_PRIME + 1],
            server_key: vec![0x12; MAX_PRIME + 1],
            ..bad
        };
        assert!(apple_response(&huge, "alex", "pw", none).is_err());
    }

    #[test]
    fn keys_are_padded_to_the_primes_length() {
        assert_eq!(padded(&BigUint::from(0x0102u16), 4), [0, 0, 1, 2]);
        assert_eq!(padded(&BigUint::from(0x010203u32), 2), [2, 3]);
    }
}
