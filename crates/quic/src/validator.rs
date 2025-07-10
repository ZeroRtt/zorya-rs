//! Alogorithm and types for [`address validation`].
//!
//! [`address validation`]: https://datatracker.ietf.org/doc/html/rfc9000#name-address-validation

use std::{
    net::SocketAddr,
    time::{Duration, SystemTime},
};

use boring::sha::Sha256;
use quiche::ConnectionId;

/// Address validation trait.
pub trait AddressValidator {
    /// Create a retry-token.
    fn mint_retry_token(
        &self,
        scid: &ConnectionId<'_>,
        dcid: &ConnectionId<'_>,
        new_scid: &ConnectionId<'_>,
        src: &SocketAddr,
    ) -> std::io::Result<Vec<u8>>;

    /// Validate the source address.
    fn validate_address<'a>(
        &self,
        scid: &ConnectionId<'_>,
        dcid: &ConnectionId<'_>,
        src: &SocketAddr,
        token: &'a [u8],
    ) -> Option<ConnectionId<'a>>;
}

/// A default implementation for [`AddressValidator`]
#[derive(Debug)]
pub struct SimpleAddressValidator([u8; 20], Duration);

impl SimpleAddressValidator {
    /// Create a new `SimpleAddressValidator` instance with token expiration interval.
    pub fn new(expiration_interval: Duration) -> Self {
        let mut seed = [0; 20];
        boring::rand::rand_bytes(&mut seed).unwrap();
        Self(seed, expiration_interval)
    }
}

#[allow(unused)]
impl AddressValidator for SimpleAddressValidator {
    fn mint_retry_token(
        &self,
        _scid: &ConnectionId<'_>,
        dcid: &ConnectionId<'_>,
        new_scid: &ConnectionId<'_>,
        src: &SocketAddr,
    ) -> std::io::Result<Vec<u8>> {
        let mut token = vec![];
        // ip
        match src.ip() {
            std::net::IpAddr::V4(ipv4_addr) => token.extend_from_slice(&ipv4_addr.octets()),
            std::net::IpAddr::V6(ipv6_addr) => token.extend_from_slice(&ipv6_addr.octets()),
        };

        let timestamp = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs();

        // timestamp
        token.extend_from_slice(&timestamp.to_be_bytes());
        // odcid
        token.extend_from_slice(dcid);

        // sha256
        let mut hasher = Sha256::new();
        // seed
        hasher.update(&self.0);
        // ip + timestamp + odcid
        hasher.update(&token);
        // new_scid
        hasher.update(&new_scid);

        token.extend_from_slice(&hasher.finish());

        Ok(token)
    }

    fn validate_address<'a>(
        &self,
        scid: &ConnectionId<'_>,
        dcid: &ConnectionId<'_>,
        src: &SocketAddr,
        token: &'a [u8],
    ) -> Option<ConnectionId<'a>> {
        let addr = match src.ip() {
            std::net::IpAddr::V4(a) => a.octets().to_vec(),
            std::net::IpAddr::V6(a) => a.octets().to_vec(),
        };

        // token length is too short.
        if addr.len() + 40 > token.len() {
            return None;
        }

        // invalid address.
        if addr != &token[..addr.len()] {
            return None;
        }

        let timestamp = Duration::from_secs(u64::from_be_bytes(
            token[addr.len()..addr.len() + 8].try_into().unwrap(),
        ));
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap();

        // timeout
        if now - timestamp > self.1 {
            return None;
        }

        let sha256 = &token[token.len() - 32..];

        // sha256
        let mut hasher = Sha256::new();
        // seed
        hasher.update(&self.0);
        // ip + timestamp + odcid
        hasher.update(&token[..token.len() - 32]);
        // new_scid
        hasher.update(&dcid);

        // sha256 check error.
        if sha256 != hasher.finish() {
            return None;
        }

        Some(ConnectionId::from_ref(
            &token[addr.len() + 8..token.len() - 32],
        ))
    }
}

#[cfg(test)]
mod tests {
    use std::{net::SocketAddr, thread::sleep, time::Duration};

    use super::*;

    fn random_conn_id() -> ConnectionId<'static> {
        let mut buf = vec![0; 20];
        boring::rand::rand_bytes(&mut buf).unwrap();

        ConnectionId::from_vec(buf)
    }

    #[test]
    fn test_default_address_validator() {
        let _validator = SimpleAddressValidator::new(Duration::from_secs(100));

        let scid = random_conn_id();
        let dcid = random_conn_id();
        let new_scid = random_conn_id();

        let src: SocketAddr = "127.0.0.1:1234".parse().unwrap();

        let token = _validator
            .mint_retry_token(&scid, &dcid, &new_scid, &src)
            .unwrap();

        assert_eq!(
            _validator.validate_address(&scid, &new_scid, &src, &token),
            Some(dcid.clone())
        );

        assert_eq!(
            _validator.validate_address(&scid, &dcid, &src, &token),
            None
        );

        assert_eq!(
            _validator.validate_address(&scid, &new_scid, &src, &token),
            Some(dcid.clone())
        );

        let src: SocketAddr = "0.0.0.0:1234".parse().unwrap();

        assert_eq!(
            _validator.validate_address(&scid, &new_scid, &src, &token),
            None
        );

        let _validator = SimpleAddressValidator::new(Duration::from_secs(1));

        let token = _validator
            .mint_retry_token(&scid, &dcid, &new_scid, &src)
            .unwrap();

        assert_eq!(
            _validator.validate_address(&scid, &new_scid, &src, &token),
            Some(dcid.clone())
        );

        sleep(Duration::from_secs(2));

        assert_eq!(
            _validator.validate_address(&scid, &new_scid, &src, &token),
            None
        );

        // timeout.
    }
}
